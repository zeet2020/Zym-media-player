//! Music visualizer: drawn by the player while an audio-only file plays (Settings › Appearance).
//!
//! Data: an mpv audio filter (`@viz`, see `filter()`) splits the sound into BANDS band-pass
//! filters and measures each one's loudness per audio frame; mpv publishes that as the filter's
//! metadata, which the event thread observes like any other property (main.rs OBSERVED), so the
//! UI never waits on mpv. mpv runs filters in bursts, so updates arrive ~18×/s; `update` smooths
//! them into motion at the frame rate.
//! ponytail: no audio-latency compensation; the bands lead the speakers by mpv's audio buffer
//! (~0.2 s). Delay the levels by `audio-buffer` if beats look early.

use std::time::Instant;

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Window, canvas, fill, hsla, linear_color_stop, linear_gradient, point, prelude::*, px, size};

pub const BANDS: usize = 16;
/// Filter label; also the property name "af-metadata/viz".
pub const LABEL: &str = "viz";
pub const PROPERTY: &str = "af-metadata/viz";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    Off,
    Bars,
    Wave,
    Circle,
    Dots,
    Stars,
}

impl Style {
    pub const ALL: [(&str, Style); 6] = [("Off", Style::Off), ("Bars", Style::Bars), ("Wave", Style::Wave), ("Circle", Style::Circle), ("Dots", Style::Dots), ("Stars", Style::Stars)];

    fn name(self) -> &'static str {
        Style::ALL.iter().find(|(_, s)| *s == self).map(|(n, _)| *n).unwrap_or("Off")
    }

    /// Saved choice ("ui"."visualizer"); Bars when never set.
    pub fn saved() -> Style {
        let name = crate::settings::get("ui").and_then(|u| u.get("visualizer").and_then(|v| v.as_str().map(str::to_string)));
        name.and_then(|n| Style::ALL.iter().find(|(l, _)| *l == n).map(|(_, s)| *s)).unwrap_or(Style::Bars)
    }

    pub fn save(self) {
        let mut ui = crate::settings::get("ui").filter(|v| v.is_object()).unwrap_or_else(|| serde_json::json!({}));
        ui["visualizer"] = self.name().into();
        crate::settings::put("ui", ui);
    }
}

/// Band centre frequencies: log-spaced 40 Hz – 16 kHz.
fn band_hz(i: usize) -> f32 {
    40. * 400f32.powf(i as f32 / (BANDS - 1) as f32)
}

/// The lavfi graph for `af add @viz:lavfi=[…]`. Output = the input as stereo. Analysis: a mono mix
/// through BANDS band-pass filters; the bands are joined to the music as extra channels (a layout
/// with distinct names, so ffmpeg doesn't warn), astats measures every channel (channels 3.. =
/// bands), and the final pan keeps only the music.
pub fn filter() -> String {
    const NAMES: [&str; 18] = ["FL", "FR", "FC", "LFE", "BL", "BR", "FLC", "FRC", "BC", "SL", "SR", "TC", "TFL", "TFC", "TFR", "TBL", "TBC", "TBR"];
    let outs: String = (0..BANDS).map(|i| format!("[b{i}]")).collect();
    let bands: String = (0..BANDS).map(|i| format!("[b{i}]bandpass=f={:.0}:width_type=o:w=0.6[c{i}];", band_hz(i))).collect();
    let joined: String = (0..BANDS).map(|i| format!("[c{i}]")).collect();
    format!(
        "@{LABEL}:lavfi=[aformat=channel_layouts=stereo,asplit=2[m][a];[a]pan=mono|c0=0.5*c0+0.5*c1,asplit={BANDS}{outs};{bands}[m]{joined}join=inputs={}:channel_layout={},astats=metadata=1:reset=1:measure_overall=none:measure_perchannel=RMS_level,pan=stereo|c0=c0|c1=c1]",
        BANDS + 1,
        NAMES[..BANDS + 2].join("+")
    )
}

/// Band loudness in dB from the metadata JSON ({"lavfi.astats.3.RMS_level":"-29.5", …}).
/// Silence is "-inf"; a missing band reads as silence.
pub fn parse(meta: &str) -> Option<[f32; BANDS]> {
    let v: serde_json::Value = serde_json::from_str(meta).ok()?;
    let mut out = [-120.; BANDS];
    let mut any = false;
    for (i, db) in out.iter_mut().enumerate() {
        if let Some(s) = v.get(format!("lavfi.astats.{}.RMS_level", i + 3)).and_then(|x| x.as_str()) {
            any = true;
            *db = s.parse::<f32>().ok().filter(|d| d.is_finite()).unwrap_or(-120.);
        }
    }
    any.then_some(out)
}

#[derive(Clone, Copy)]
struct Star {
    x: f32,
    y: f32,
    z: f32,
}

pub struct Viz {
    pub style: Style,
    /// `@viz` is in mpv's af chain.
    pub active: bool,
    /// Smoothed band levels, 0..=1.
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    peak_fall: [f32; BANDS],
    /// dB that maps to a full bar: follows the loudest band down slowly (auto gain).
    ceil: f32,
    last_meta: String,
    targets: [f32; BANDS],
    bass_avg: f32,
    /// 1 on a beat, decays to 0.
    pulse: f32,
    since_beat: f32,
    /// Colour wheel position, turns faster when the music is louder.
    hue: f32,
    /// Wave phase.
    time: f32,
    stars: Vec<Star>,
    seed: u32,
    last: Instant,
}

impl Viz {
    pub fn new() -> Self {
        let mut v = Viz {
            style: Style::saved(),
            active: false,
            levels: [0.; BANDS],
            peaks: [0.; BANDS],
            peak_fall: [0.; BANDS],
            ceil: -20.,
            last_meta: String::new(),
            targets: [0.; BANDS],
            bass_avg: 0.,
            pulse: 0.,
            since_beat: 0.,
            hue: 0.,
            time: 0.,
            stars: Vec::new(),
            seed: 0x2545_f491,
            last: Instant::now(),
        };
        v.stars = (0..420).map(|_| v.new_star(true)).collect();
        v
    }

    /// xorshift: plenty for star positions, no dependency.
    fn rand(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed as f32 / u32::MAX as f32) * 2. - 1.
    }

    fn new_star(&mut self, anywhere: bool) -> Star {
        let z = if anywhere { (self.rand() + 1.) / 2. } else { 1. };
        Star { x: self.rand(), y: self.rand(), z: z.max(0.05) }
    }

    /// Advance one frame. `meta` = latest filter metadata (None = no data yet / paused).
    pub fn update(&mut self, meta: Option<&str>, playing: bool) {
        let dt = self.last.elapsed().as_secs_f32().min(0.1);
        self.last = Instant::now();
        if let Some(m) = meta.filter(|m| *m != self.last_meta) {
            self.last_meta = m.to_string();
            if let Some(db) = parse(m) {
                let loudest = db.iter().cloned().fold(-120., f32::max);
                // Auto gain: jump up to a louder band, sink 3 dB/s, never below -40 dB.
                self.ceil = if loudest > self.ceil { loudest } else { (self.ceil - 3. * dt).max(loudest).max(-40.) };
                for (t, d) in self.targets.iter_mut().zip(db) {
                    *t = ((d - (self.ceil - 42.)) / 42.).clamp(0., 1.);
                }
            }
        }
        if !playing {
            self.targets = [0.; BANDS];
        }
        for i in 0..BANDS {
            let (l, t) = (self.levels[i], self.targets[i]);
            // Fast attack, slower release.
            let rate = if t > l { 18. } else { 5. };
            self.levels[i] = l + (t - l) * (1. - (-rate * dt).exp());
            if self.levels[i] >= self.peaks[i] {
                self.peaks[i] = self.levels[i];
                self.peak_fall[i] = 0.;
            } else {
                self.peak_fall[i] += 1.6 * dt; // gravity
                self.peaks[i] = (self.peaks[i] - self.peak_fall[i] * dt).max(self.levels[i]);
            }
        }
        // Beat: bass well above its recent average.
        let bass = self.levels[..4].iter().sum::<f32>() / 4.;
        self.bass_avg += (bass - self.bass_avg) * (1. - (-1.5 * dt).exp());
        self.since_beat += dt;
        if bass > 0.35 && bass > self.bass_avg * 1.25 && self.since_beat > 0.25 {
            self.pulse = 1.;
            self.since_beat = 0.;
        }
        self.pulse *= (-5. * dt).exp();
        let energy = self.levels.iter().sum::<f32>() / BANDS as f32;
        self.hue = (self.hue + dt * (0.01 + 0.12 * energy) + self.pulse * dt * 0.3).fract();
        self.time += dt;
        if self.style == Style::Stars {
            let speed = 0.12 + 0.5 * energy + 1.6 * self.pulse;
            for i in 0..self.stars.len() {
                self.stars[i].z -= speed * dt;
                if self.stars[i].z <= 0.02 {
                    self.stars[i] = self.new_star(false);
                }
            }
        }
    }

    /// Level at fraction `x` (0..=1) across the bands, smoothly interpolated.
    fn at(v: &[f32; BANDS], x: f32) -> f32 {
        let p = x.clamp(0., 1.) * (BANDS - 1) as f32;
        let (i, f) = (p.floor() as usize, p.fract());
        let (a, b) = (v[i], v[(i + 1).min(BANDS - 1)]);
        let f = f * f * (3. - 2. * f);
        a + (b - a) * f
    }

    /// The visual, filling its parent.
    pub fn element(&self, accent: Hsla) -> impl IntoElement {
        let snap = Snap {
            style: self.style,
            levels: self.levels,
            peaks: self.peaks,
            pulse: self.pulse,
            hue: (accent.h + self.hue).fract(),
            time: self.time,
            stars: self.stars.clone(),
        };
        canvas(|_, _, _| {}, move |bounds, _, window, _| snap.paint(bounds, window)).size_full()
    }
}

/// What one frame draws (the canvas closure must own its data).
struct Snap {
    style: Style,
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    pulse: f32,
    hue: f32,
    time: f32,
    stars: Vec<Star>,
}

impl Snap {
    /// Colour at position `x` (0..=1) along the spectrum: the hue fans out from the current one.
    fn color(&self, x: f32, light: f32, alpha: f32) -> Hsla {
        hsla((self.hue + x * 0.35).fract(), 0.85, light, alpha)
    }

    fn paint(&self, b: Bounds<Pixels>, window: &mut Window) {
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        let (ox, oy) = (f32::from(b.origin.x), f32::from(b.origin.y));
        let p = |x: f32, y: f32| point(px(ox + x), px(oy + y));
        match self.style {
            Style::Off => {}
            Style::Bars => {
                // Wide bars, 3 px apart, rounded tops, each a vertical gradient (the look of
                // MusicMotion's bars, MIT), with falling peak caps and a faint reflection.
                let gap = 3.;
                let n = ((w * 0.8 / 22.) as usize).clamp(12, 48);
                let bw = ((w * 0.8 - gap * (n - 1) as f32) / n as f32).max(2.);
                let (left, base, max_h) = ((w - (bw + gap) * n as f32 + gap) / 2., h * 0.78, h * 0.5);
                let r = (bw / 2.).min(4.);
                for i in 0..n {
                    let x = i as f32 / (n - 1) as f32;
                    let bh = (Viz::at(&self.levels, x) * max_h).max(3.);
                    let bx = left + i as f32 * (bw + gap);
                    let grad = linear_gradient(0., linear_color_stop(self.color(x, 0.5, 0.95), 0.), linear_color_stop(self.color(x + 0.6, 0.62, 0.95), 1.));
                    window.paint_quad(
                        fill(Bounds::new(p(bx, base - bh), size(px(bw), px(bh))), grad)
                            .corner_radii(gpui::Corners { top_left: px(r), top_right: px(r), bottom_left: px(0.), bottom_right: px(0.) }),
                    );
                    window.paint_quad(fill(Bounds::new(p(bx, base + 3.), size(px(bw), px(bh * 0.25))), self.color(x, 0.5, 0.12)));
                    let pk = base - Viz::at(&self.peaks, x) * max_h - 7.;
                    window.paint_quad(fill(Bounds::new(p(bx, pk), size(px(bw), px(3.))), self.color(x + 0.6, 0.8, 0.95)).corner_radii(px(1.5)));
                }
            }
            Style::Wave => {
                let mid = h / 2.;
                let amp = h * 0.28 * (1. + 0.25 * self.pulse);
                let steps = 240;
                // Each band adds a sine at its own speed, so the line moves like the music sounds.
                let y = |x: f32| -> f32 {
                    let mut s = 0.;
                    for (i, lv) in self.levels.iter().enumerate() {
                        let k = 1.5 + i as f32 * 1.7;
                        s += lv * (x * k * std::f32::consts::TAU + self.time * (1. + i as f32 * 0.6)).sin() / (1. + i as f32 * 0.15);
                    }
                    let edge = (x * std::f32::consts::PI).sin(); // pinned at both ends
                    mid - s / 3. * amp * edge
                };
                // Glow: wide faint strokes under a thin bright one.
                for (width, light, alpha) in [(18., 0.5, 0.06), (9., 0.55, 0.14), (4., 0.65, 0.4), (2., 0.85, 1.)] {
                    let mut path = PathBuilder::stroke(px(width));
                    path.move_to(p(0., y(0.)));
                    for s in 1..=steps {
                        let x = s as f32 / steps as f32;
                        path.line_to(p(x * w, y(x)));
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, self.color(0.5, light, alpha));
                    }
                }
            }
            Style::Circle => {
                let (cx, cy) = (w / 2., h / 2.);
                let r0 = w.min(h) * 0.17 * (1. + 0.12 * self.pulse);
                let len = w.min(h) * 0.26;
                let n = 96;
                for i in 0..n {
                    // Mirrored: low notes at the top, high notes at the bottom, on both sides.
                    let half = i as f32 / n as f32 * 2.;
                    let x = if half <= 1. { half } else { 2. - half };
                    let lv = Viz::at(&self.levels, x);
                    let a = i as f32 / n as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                    let (s, c) = a.sin_cos();
                    let r1 = r0 + 3. + lv * len;
                    let mut path = PathBuilder::stroke(px((r0 * std::f32::consts::TAU / n as f32 * 0.55).max(2.)));
                    path.move_to(p(cx + c * (r0 + 3.), cy + s * (r0 + 3.)));
                    path.line_to(p(cx + c * r1, cy + s * r1));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, self.color(x, 0.58, 0.9));
                    }
                }
                let mut ring = PathBuilder::stroke(px(2.));
                ring.move_to(p(cx + r0, cy));
                for s in 1..=72 {
                    let a = s as f32 / 72. * std::f32::consts::TAU;
                    ring.line_to(p(cx + a.cos() * r0, cy + a.sin() * r0));
                }
                if let Ok(ring) = ring.build() {
                    window.paint_path(ring, self.color(0., 0.75, 0.5 + 0.5 * self.pulse));
                }
            }
            Style::Dots => {
                // A row of glowing dots on the centre line; each swells with its part of the spectrum.
                let n = ((w * 0.8 / 34.) as usize).clamp(10, 32);
                let step = w * 0.8 / n as f32;
                let max_r = (step * 0.5).min(h * 0.2);
                for i in 0..n {
                    let x = i as f32 / (n - 1) as f32;
                    let lv = Viz::at(&self.levels, x);
                    let (dx, dy) = (w * 0.1 + step * (i as f32 + 0.5), h / 2.);
                    let r = (2. + lv * max_r) * (1. + 0.15 * self.pulse);
                    for (scale, light, alpha) in [(2.2, 0.5, 0.08 * lv), (1.5, 0.55, 0.18 * lv), (1., 0.6, 0.95)] {
                        let rr = r * scale;
                        window.paint_quad(fill(Bounds::new(p(dx - rr, dy - rr), size(px(rr * 2.), px(rr * 2.))), self.color(x, light, alpha)).corner_radii(px(rr)));
                    }
                }
            }
            Style::Stars => {
                let (cx, cy) = (w / 2., h / 2.);
                let spread = w.max(h) * 0.5;
                for s in &self.stars {
                    let (x, y) = (cx + s.x / s.z * spread * 0.5, cy + s.y / s.z * spread * 0.5);
                    if !(0. ..w).contains(&x) || !(0. ..h).contains(&y) {
                        continue;
                    }
                    let near = 1. - s.z;
                    let r = 0.6 + near * 3.2 * (1. + 0.6 * self.pulse);
                    window.paint_quad(fill(Bounds::new(p(x - r, y - r), size(px(r * 2.), px(r * 2.))), self.color(s.x.abs(), 0.7 + 0.25 * near, 0.25 + 0.75 * near)).corner_radii(px(r)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_and_parse() {
        let g = filter();
        assert!(g.starts_with("@viz:lavfi=["), "{g}");
        assert_eq!(g.matches("bandpass=").count(), BANDS);
        assert!(g.contains(&format!("join=inputs={}", BANDS + 1)));
        assert_eq!(band_hz(0).round(), 40.);
        assert_eq!(band_hz(BANDS - 1).round(), 16000.);

        let meta = r#"{"lavfi.astats.1.RMS_level":"-21.4","lavfi.astats.2.RMS_level":"-21.4","lavfi.astats.3.RMS_level":"-29.5","lavfi.astats.4.RMS_level":"-inf"}"#;
        let db = parse(meta).unwrap();
        assert_eq!(db[0], -29.5, "channel 3 = first band");
        assert_eq!(db[1], -120., "-inf = silence");
        assert_eq!(db[5], -120., "missing band = silence");
        assert!(parse("{}").is_none() && parse("junk").is_none());
    }

    #[test]
    fn levels_follow_and_peaks_fall() {
        let mut v = Viz::new();
        let loud = format!("{{{}}}", (0..BANDS).map(|i| format!("\"lavfi.astats.{}.RMS_level\":\"-20\"", i + 3)).collect::<Vec<_>>().join(","));
        for _ in 0..30 {
            v.last = Instant::now() - std::time::Duration::from_millis(16);
            v.update(Some(&loud), true);
        }
        assert!(v.levels[0] > 0.9, "rises to full: {}", v.levels[0]);
        for _ in 0..60 {
            v.last = Instant::now() - std::time::Duration::from_millis(16);
            v.update(None, false);
        }
        assert!(v.levels[0] < 0.05, "paused -> falls: {}", v.levels[0]);
        assert!(v.peaks[0] >= v.levels[0]);
    }
}
