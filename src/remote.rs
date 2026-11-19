//! FTP / SFTP folder listing for source plugins (sandboxed plugins only get HTTP).
//! Exposed to plugins as the Extism host function `zym_list_dir`, which enforces the calling
//! plugin's manifest `hosts` before connecting. Playback itself is mpv's own ftp:// / sftp://.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub scheme: String, // "ftp" | "sftp"
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// `ftp://host[:port]/path` / `sftp://...`. Credentials in the URL are rejected (pass them separately).
pub fn parse_url(url: &str) -> Result<Target, String> {
    let (scheme, rest) = url.split_once("://").ok_or("URL needs a scheme (ftp:// or sftp://)")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "ftp" => 21,
        "sftp" => 22,
        s => return Err(format!("unsupported scheme {s}:// (use ftp:// or sftp://)")),
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if authority.contains('@') {
        return Err("put the username/password in settings, not in the URL".into());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h, p.parse().map_err(|_| format!("bad port {p:?}"))?),
        _ => (authority, default_port),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    if host.is_empty() {
        return Err("URL has no host".into());
    }
    let path = if path.is_empty() { "/".to_string() } else { crate_percent_decode(path) };
    Ok(Target { scheme, host, port, path })
}

fn crate_percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Manifest `hosts` check: "*", exact host, or "*.example.com" (subdomains only).
pub fn host_allowed(host: &str, patterns: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    patterns.iter().any(|p| {
        let p = p.to_ascii_lowercase();
        p == "*" || p == host || p.strip_prefix("*.").is_some_and(|d| host.ends_with(&format!(".{d}")))
    })
}

pub fn list(t: &Target, user: &str, pass: &str) -> Result<Vec<Entry>, String> {
    let mut entries = match t.scheme.as_str() {
        "ftp" => list_ftp(t, user, pass),
        _ => list_sftp(t, user, pass),
    }?;
    entries.retain(|e| e.name != "." && e.name != "..");
    Ok(entries)
}

fn list_ftp(t: &Target, user: &str, pass: &str) -> Result<Vec<Entry>, String> {
    use std::net::ToSocketAddrs;
    use suppaftp::list::ListParser;
    let addr = (t.host.as_str(), t.port).to_socket_addrs().map_err(|e| format!("can't resolve {}: {e}", t.host))?.next().ok_or("no address")?;
    let mut ftp = suppaftp::FtpStream::connect_timeout(addr, TIMEOUT).map_err(|e| format!("FTP connect failed: {e}"))?;
    let (u, p) = if user.is_empty() { ("anonymous", "anonymous@") } else { (user, pass) };
    ftp.login(u, p).map_err(|e| format!("FTP login failed — check username and password ({e})"))?;
    // MLSD is machine-readable; fall back to LIST (unix, then DOS format) on old servers.
    let entries = match ftp.mlsd(Some(&t.path)) {
        Ok(lines) => lines.iter().filter_map(|l| ListParser::parse_mlsd(l).ok()).collect::<Vec<_>>(),
        Err(_) => ftp
            .list(Some(&t.path))
            .map_err(|e| format!("FTP listing failed for {}: {e}", t.path))?
            .iter()
            .filter_map(|l| ListParser::parse_posix(l).or_else(|_| ListParser::parse_dos(l)).ok())
            .collect(),
    };
    ftp.quit().ok();
    Ok(entries
        .into_iter()
        .map(|f| Entry { name: f.name().to_string(), is_dir: f.is_directory(), size: f.is_file().then_some(f.size() as u64) })
        .collect())
}

// ---------------------------------------------------------------- SFTP

/// Trust-on-first-use host keys: "<host>:<port> <SHA256 fingerprint>" per line.
fn known_hosts_path() -> std::path::PathBuf {
    crate::plugins_dir().parent().unwrap().join("known_hosts")
}

/// Ok(true) = trusted (stored or newly stored), Err = key changed.
pub fn tofu_check(store: &std::path::Path, host_port: &str, fingerprint: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(store).unwrap_or_default();
    match text.lines().find_map(|l| l.split_once(' ').filter(|(h, _)| *h == host_port)) {
        Some((_, fp)) if fp == fingerprint => Ok(()),
        Some((_, fp)) => Err(format!(
            "SFTP host key for {host_port} CHANGED (was {fp}, now {fingerprint}). Refusing to connect. \
             If the server was reinstalled, remove its line from {}.",
            store.display()
        )),
        None => {
            if let Some(dir) = store.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            let line = format!("{host_port} {fingerprint}\n");
            std::fs::write(store, text + &line).map_err(|e| format!("can't save host key: {e}"))?;
            eprintln!("sftp: trusted new host key {host_port} {fingerprint}");
            Ok(())
        }
    }
}

struct Tofu {
    host_port: String,
    rejection: Arc<Mutex<Option<String>>>,
}

impl russh::client::Handler for Tofu {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            // ponytail: SSH certificates need a configured CA to trust; refuse until someone needs it.
            *self.rejection.lock().unwrap() = Some("SFTP server presented an SSH certificate; not supported yet".into());
            return Ok(false);
        };
        let fp = key.fingerprint(russh::keys::HashAlg::Sha256).to_string();
        match tofu_check(&known_hosts_path(), &self.host_port, &fp) {
            Ok(()) => Ok(true),
            Err(e) => {
                *self.rejection.lock().unwrap() = Some(e);
                Ok(false)
            }
        }
    }
}

fn list_sftp(t: &Target, user: &str, pass: &str) -> Result<Vec<Entry>, String> {
    if user.is_empty() {
        return Err("SFTP needs a username in settings".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    let rejection = Arc::new(Mutex::new(None));
    let handler = Tofu { host_port: format!("{}:{}", t.host, t.port), rejection: rejection.clone() };
    // The timeout must be created inside the runtime (it registers a timer with it).
    let result = rt.block_on(async { tokio::time::timeout(TIMEOUT, async {
        let config = Arc::new(russh::client::Config { inactivity_timeout: Some(TIMEOUT), ..Default::default() });
        let mut session = russh::client::connect(config, (t.host.as_str(), t.port), handler).await.map_err(|e| format!("SFTP connect failed: {e}"))?;
        let auth = session.authenticate_password(user, pass).await.map_err(|e| format!("SFTP login error: {e}"))?;
        if !auth.success() {
            return Err("SFTP login failed — check username and password".to_string());
        }
        let channel = session.channel_open_session().await.map_err(|e| e.to_string())?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| e.to_string())?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|e| format!("SFTP start failed: {e}"))?;
        let dir = sftp.read_dir(t.path.clone()).await.map_err(|e| format!("SFTP listing failed for {}: {e}", t.path))?;
        let out = dir
            .map(|e| {
                let meta = e.metadata();
                Entry { name: e.file_name(), is_dir: meta.is_dir(), size: (!meta.is_dir()).then(|| meta.len()) }
            })
            .collect::<Vec<_>>();
        sftp.close().await.ok();
        Ok(out)
    }).await });
    if let Some(why) = rejection.lock().unwrap().take() {
        return Err(why); // host-key mismatch beats the generic "connect failed"
    }
    result.map_err(|_| format!("SFTP timed out after {}s", TIMEOUT.as_secs()))?
}

// ---------------------------------------------------------------- host function

/// Request: {"url", "username"?, "password"?} -> {"entries":[{name,is_dir,size}]} | {"error"}.
pub fn handle_request(req: &str, allowed_hosts: &[String]) -> Value {
    let run = || -> Result<Value, String> {
        let v: Value = serde_json::from_str(req).map_err(|e| format!("bad request: {e}"))?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let target = parse_url(&s("url"))?;
        if !host_allowed(&target.host, allowed_hosts) {
            return Err(format!("host {} is not in this plugin's manifest `hosts`", target.host));
        }
        let entries = list(&target, &s("username"), &s("password"))?;
        Ok(json!({ "entries": entries.iter().map(|e| json!({ "name": e.name, "is_dir": e.is_dir, "size": e.size })).collect::<Vec<_>>() }))
    };
    run().unwrap_or_else(|e| json!({ "error": e }))
}

pub fn host_function(allowed_hosts: Vec<String>) -> extism::Function {
    extism::Function::new(
        "zym_list_dir",
        [extism::PTR],
        [extism::PTR],
        extism::UserData::new(allowed_hosts),
        |plugin: &mut extism::CurrentPlugin, inputs: &[extism::Val], outputs: &mut [extism::Val], data: extism::UserData<Vec<String>>| {
            let req: String = plugin.memory_get_val(&inputs[0])?;
            let hosts = data.get()?.lock().unwrap().clone();
            let resp = handle_request(&req, &hosts).to_string();
            plugin.memory_set_val(&mut outputs[0], resp)?;
            Ok(())
        },
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn urls() {
        let t = parse_url("sftp://nas.local:2222/Media/My%20Movies").unwrap();
        assert_eq!((t.scheme.as_str(), t.host.as_str(), t.port, t.path.as_str()), ("sftp", "nas.local", 2222, "/Media/My Movies"));
        assert_eq!(parse_url("ftp://h").unwrap().port, 21);
        assert_eq!(parse_url("ftp://h").unwrap().path, "/");
        assert_eq!(parse_url("sftp://[::1]:22/x").unwrap().host, "::1");
        assert!(parse_url("sftp://u:p@h/").is_err());
        assert!(parse_url("http://h/").is_err());
        assert!(parse_url("ftp:///x").is_err());
    }

    #[test]
    fn host_patterns() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(host_allowed("any.host", &p(&["*"])));
        assert!(host_allowed("NAS.local", &p(&["nas.local"])));
        assert!(host_allowed("a.example.com", &p(&["*.example.com"])));
        assert!(!host_allowed("example.com", &p(&["*.example.com"])));
        assert!(!host_allowed("evil.com", &p(&["nas.local"])));
        let r = handle_request(r#"{"url":"ftp://evil.com/"}"#, &p(&["nas.local"]));
        assert!(r["error"].as_str().unwrap().contains("not in this plugin's manifest"));
    }

    #[test]
    fn tofu() {
        let store = std::env::temp_dir().join(format!("zym-known-hosts-{}", std::process::id()));
        std::fs::remove_file(&store).ok();
        assert!(tofu_check(&store, "h:22", "SHA256:aaa").is_ok()); // first use: stored
        assert!(tofu_check(&store, "h:22", "SHA256:aaa").is_ok()); // same key
        assert!(tofu_check(&store, "h:22", "SHA256:bbb").unwrap_err().contains("CHANGED"));
        assert!(tofu_check(&store, "other:22", "SHA256:bbb").is_ok()); // per host:port
        std::fs::remove_file(&store).ok();
    }

    /// Tiny in-process FTP server (USER/PASS alice:secret, EPSV/PASV, MLSD of /media). Returns its port.
    pub(crate) fn spawn_ftp_server() -> u16 {
        use std::io::{BufRead, BufReader, Write};
        let ctrl = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = ctrl.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (s, _) = ctrl.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            writeln!(w, "220 hi\r").unwrap();
            let data = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let dp = data.local_addr().unwrap().port();
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 0 {
                let cmd = line.trim().to_string();
                line.clear();
                let verb = cmd.split(' ').next().unwrap_or("").to_ascii_uppercase();
                match verb.as_str() {
                    "USER" => writeln!(w, "331 pw\r").unwrap(),
                    "PASS" => writeln!(w, "{}\r", if cmd == "PASS secret" { "230 ok" } else { "530 no" }).unwrap(),
                    "TYPE" => writeln!(w, "200 ok\r").unwrap(),
                    "EPSV" => writeln!(w, "229 Entering Extended Passive Mode (|||{dp}|)\r").unwrap(),
                    "PASV" => writeln!(w, "227 Entering Passive Mode (127,0,0,1,{},{})\r", dp >> 8, dp & 255).unwrap(),
                    "MLSD" => {
                        writeln!(w, "150 here\r").unwrap();
                        let (mut d, _) = data.accept().unwrap();
                        write!(d, "type=cdir;modify=20240101000000; .\r\ntype=dir;modify=20240101000000; Shows\r\ntype=file;size=1048576;modify=20240101000000; Ep 1.mkv\r\n").unwrap();
                        drop(d);
                        writeln!(w, "226 done\r").unwrap();
                    }
                    "QUIT" => {
                        writeln!(w, "221 bye\r").unwrap();
                        break;
                    }
                    _ => writeln!(w, "502 no\r").unwrap(),
                }
            }
        });
        port
    }

    /// Manual: real SSH handshake + TOFU + login failure against $ZYM_TEST_SFTP_HOST (e.g. 127.0.0.1).
    /// Run with XDG_CONFIG_HOME pointed at a temp dir so the known_hosts it writes is throwaway.
    #[test]
    #[ignore]
    fn sftp_handshake_live() {
        let Ok(host) = std::env::var("ZYM_TEST_SFTP_HOST") else { return };
        let t = parse_url(&format!("sftp://{host}/")).unwrap();
        let err = list(&t, "zym-test-no-such-user", "x").unwrap_err();
        assert!(err.contains("login failed"), "{err}");
        let kh = std::fs::read_to_string(known_hosts_path()).unwrap();
        assert!(kh.starts_with(&format!("{host}:22 SHA256:")), "{kh}");
        // A changed key must now be refused.
        let store = known_hosts_path();
        std::fs::write(&store, format!("{host}:22 SHA256:not-the-real-key\n")).unwrap();
        let err = list(&t, "zym-test-no-such-user", "x").unwrap_err();
        assert!(err.contains("CHANGED"), "{err}");
        eprintln!("ok: handshake, TOFU store, login-failed and key-change refusal");
    }

    /// Live FTP listing against the in-process server.
    #[test]
    fn ftp_list_live() {
        let port = spawn_ftp_server();
        let t = parse_url(&format!("ftp://127.0.0.1:{port}/media")).unwrap();
        let mut e = list(&t, "alice", "secret").unwrap();
        e.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(e, vec![
            Entry { name: "Ep 1.mkv".into(), is_dir: false, size: Some(1048576) },
            Entry { name: "Shows".into(), is_dir: true, size: None },
        ]);
    }
}
