//! "Update available" in About: asks GitHub for the latest published release when About opens.
//! Nothing runs at startup or in the background; the request carries only a generic User-Agent.

use std::time::Duration;

use gpui::{Context, Div, div, prelude::*};

use crate::theme::t;
use crate::{Player, VERSION, chip};

#[derive(Clone, Default, PartialEq, Debug)]
pub enum Update {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available { version: String, url: String },
    Failed(String),
}

/// "https://github.com/owner/repo" (Cargo.toml `repository`) -> API URL of its latest release.
fn latest_release_api() -> String {
    let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/').trim_start_matches("https://github.com/");
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

/// (tag, release page URL) of the latest non-draft, non-prerelease release.
fn fetch_latest() -> Result<(String, String), String> {
    let agent = ureq::Agent::new_with_config(ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).https_only(true).build());
    let mut resp = agent
        .get(&latest_release_api())
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", &format!("zym-media-player/{VERSION}"))
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp.body_mut().with_config().limit(1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string).ok_or_else(|| format!("no {k} in release"));
    Ok((field("tag_name")?, field("html_url")?))
}

/// "v1.2.3" / "1.2.3" -> (1, 2, 3); missing parts are 0, anything after '-' or '+' is ignored.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut n = core.split('.').map(|p| p.parse::<u64>());
    Some((n.next()?.ok()?, n.next().unwrap_or(Ok(0)).ok()?, n.next().unwrap_or(Ok(0)).ok()?))
}

/// Is release tag `latest` newer than `current`? Unparseable tags never count as updates.
fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

impl Player {
    /// Start a check unless one already ran (or is running) this session.
    pub fn check_for_update(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update, Update::Checking | Update::UpToDate | Update::Available { .. }) {
            return;
        }
        self.update = Update::Checking;
        let task = cx.background_executor().spawn(async { fetch_latest() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |p, cx| {
                p.update = match result {
                    Ok((tag, url)) if is_newer(&tag, VERSION) => Update::Available { version: tag.trim_start_matches('v').to_string(), url },
                    Ok(_) => Update::UpToDate,
                    Err(e) => {
                        crate::log::warn("update", &format!("check failed: {e}"));
                        Update::Failed(e)
                    }
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The "Updates" row of the About dialog.
    pub fn update_row(&self, cx: &mut Context<Self>) -> Div {
        let row = div().flex().items_center().justify_between().gap_4().child(div().text_color(t().muted()).child("Updates"));
        match &self.update {
            Update::Unknown | Update::Checking => row.child(div().text_color(t().muted()).child("Checking…")),
            Update::UpToDate => row.child("✓ Up to date"),
            Update::Available { version, url } => {
                let url = url.clone();
                row.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_color(t().accent()).child(format!("Version {version} is available")))
                        .child(chip("update-get", "Download", true).on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))),
                )
            }
            Update::Failed(_) => row.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(t().error()).child("Couldn't check"))
                    .child(chip("update-retry", "Retry", false).on_click(cx.listener(|this, _, _, cx| {
                        this.update = Update::Unknown;
                        this.check_for_update(cx);
                    }))),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("v0.2", "0.1.9"), "missing patch = 0");
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.1", "0.1.1"), "same version");
        assert!(!is_newer("v0.1.0", "0.1.1"), "older release");
        assert!(!is_newer("v0.1.10-rc1", "0.1.10"), "pre-release suffix ignored, same core");
        assert!(is_newer("v0.1.10", "0.1.9"), "numeric, not string, order");
        assert!(!is_newer("nightly", "0.1.1"), "unparseable tag never offered");
        assert_eq!(latest_release_api(), "https://api.github.com/repos/zeet2020/Zym-media-player/releases/latest");
    }
}
