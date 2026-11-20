//! Music visualizer: drawn by the player while an audio-only file plays (Settings › Appearance).
//!
//! Data: an mpv audio filter (`@viz`, see `filter()`) splits the sound into BANDS band-pass
//! filters and measures each one's loudness per audio frame; mpv publishes that as the filter's
//! metadata, which the event thread observes like any other property (main.rs OBSERVED), so the
//! UI never waits on mpv. mpv runs filters in bursts, so updates arrive ~18×/s; `update` smooths
//! them into motion at the frame rate.
//! ponytail: no audio-latency compensation; the bands lead the speakers by mpv's audio buffer
//! (~0.2 s). Delay the levels by `audio-buffer` if beats look early.

mod beat;

use std::collections::VecDeque;
use std::f32::consts::{PI, TAU};
use std::time::Instant;

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Window, canvas, fill, hsla, linear_color_stop, linear_gradient, point, prelude::*, px, size};

pub const BANDS: usize = 16;
/// Filter label; also the property name "af-metadata/viz".
pub const LABEL: &str = "viz";
pub const PROPERTY: &str = "af-metadata/viz";
/// Updates of band levels kept for Horizon (~2.6 s).
const HISTORY: usize = 48;
/// Seconds a Circle shockwave lives.
const SHOCK_LIFE: f32 = 1.2;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    Off,
    Bars,
    Led,
    Wave,
    Circle,
    Dots,
    Horizon,
    Stars,
}

impl Style {
    pub const ALL: [(&str, Style); 8] = [
        ("Off", Style::Off),
        ("Bars", Style::Bars),
        ("LED", Style::Led),
        ("Wave", Style::Wave),
        ("Circle", Style::Circle),
        ("Dots", Style::Dots),
        ("Horizon", Style::Horizon),
        ("Stars", Style::Stars),
    ];

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
    /// Seconds a peak still holds before it falls.
    peak_hold: [f32; BANDS],
    peak_fall: [f32; BANDS],
    /// dB that maps to a full bar: follows the loudest band down slowly (auto gain).
    ceil: f32,
    last_meta: String,
    targets: [f32; BANDS],
    /// When the current `targets` arrived.
    packet_at: Instant,
    beats: beat::Beats,
    /// Recent band levels, newest first (Horizon's mountains).
    history: VecDeque<[f32; BANDS]>,
    /// Age in seconds of each kick's shockwave (Circle).
    shocks: Vec<f32>,
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
            peak_hold: [0.; BANDS],
            peak_fall: [0.; BANDS],
            ceil: -20.,
            last_meta: String::new(),
            targets: [0.; BANDS],
            packet_at: Instant::now(),
            beats: beat::Beats::new(),
            history: VecDeque::new(),
            shocks: Vec::new(),
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
                let since = self.packet_at.elapsed().as_secs_f32().min(0.5);
                self.packet_at = Instant::now();
                if self.beats.feed(&self.targets, since) {
                    self.shocks.push(0.);
                }
                if self.beats.drop == 1. {
                    self.hue = (self.hue + 0.33).fract(); // a drop changes the colours
                }
                self.history.push_front(self.targets);
                self.history.truncate(HISTORY);
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
            // Peaks hold 0.35 s, then fall faster and faster.
            if self.levels[i] >= self.peaks[i] {
                self.peaks[i] = self.levels[i];
                self.peak_hold[i] = 0.35;
                self.peak_fall[i] = 0.;
            } else if self.peak_hold[i] > 0. {
                self.peak_hold[i] -= dt;
            } else {
                self.peak_fall[i] += 1.8 * dt;
                self.peaks[i] = (self.peaks[i] - self.peak_fall[i] * dt).max(self.levels[i]);
            }
        }
        self.beats.fade(dt);
        for a in &mut self.shocks {
            *a += dt;
        }
        self.shocks.retain(|a| *a < SHOCK_LIFE);
        let kick = self.beats.kick.env;
        let energy = self.levels.iter().sum::<f32>() / BANDS as f32;
        self.hue = (self.hue + dt * (0.01 + 0.12 * energy) + kick * dt * 0.3).fract();
        self.time += dt;
        if self.style == Style::Stars {
            let speed = 0.12 + 0.5 * energy + 1.6 * kick;
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
        let b = &self.beats;
        let snap = Snap {
            style: self.style,
            levels: self.levels,
            peaks: self.peaks,
            pulse: b.kick.env,
            snare: b.snare.env,
            hat: b.hat.env,
            drop: b.drop,
            bpm: b.bpm,
            hue: (accent.h + self.hue).fract(),
            time: self.time,
            stars: if matches!(self.style, Style::Stars | Style::Horizon) { self.stars.clone() } else { Vec::new() },
            history: if self.style == Style::Horizon { self.history.iter().copied().collect() } else { Vec::new() },
            // Fraction of the way to the next update (~55 ms), so the mountains glide instead of stepping.
            scroll: (self.packet_at.elapsed().as_secs_f32() / 0.055).min(1.),
            shocks: self.shocks.clone(),
        };
        canvas(|_, _, _| {}, move |bounds, _, window, _| snap.paint(bounds, window)).size_full()
    }
}

/// What one frame draws (the canvas closure must own its data).
struct Snap {
    style: Style,
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    /// Kick envelope (1 on a kick).
    pulse: f32,
    snare: f32,
    hat: f32,
    drop: f32,
    bpm: f32,
    hue: f32,
    time: f32,
    stars: Vec<Star>,
    history: Vec<[f32; BANDS]>,
    scroll: f32,
    shocks: Vec<f32>,
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
        // Stroke a polyline.
        let line = |window: &mut Window, pts: &[(f32, f32)], width: f32, color: Hsla| {
            let mut path = PathBuilder::stroke(px(width));
            path.move_to(p(pts[0].0, pts[0].1));
            for &(x, y) in &pts[1..] {
                path.line_to(p(x, y));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        };
        let circle = |window: &mut Window, x: f32, y: f32, r: f32, color: Hsla| {
            window.paint_quad(fill(Bounds::new(p(x - r, y - r), size(px(r * 2.), px(r * 2.))), color).corner_radii(px(r)));
        };
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
            Style::Led => {
                // Classic LED meter: columns of segments lit up to the level, green to red,
                // unlit segments faintly visible, the peak segment held then falling.
                let cols = ((w * 0.8 / 26.) as usize).clamp(12, 32);
                let segs = 24;
                let (aw, ah) = (w * 0.8, h * 0.56);
                let (cw, sh) = (aw / cols as f32, ah / segs as f32);
                let (left, bottom) = ((w - aw) / 2., h * 0.8);
                for c in 0..cols {
                    let x = c as f32 / (cols - 1) as f32;
                    let lit = Viz::at(&self.levels, x) * segs as f32;
                    let peak = ((Viz::at(&self.peaks, x) * segs as f32).ceil() as usize).min(segs - 1);
                    for sgi in 0..segs {
                        let y = sgi as f32 / (segs - 1) as f32;
                        let color = hsla(0.33 * (1. - y) * (1. - y * 0.1), 0.9, 0.5, 1.);
                        let on = (lit - sgi as f32).clamp(0., 1.);
                        let is_peak = sgi == peak && peak > 0;
                        let alpha = if is_peak { 1. } else { 0.08 + 0.92 * on };
                        let (sx, sy) = (left + c as f32 * cw + 2., bottom - (sgi + 1) as f32 * sh + 1.5);
                        let seg = Bounds::new(p(sx, sy), size(px(cw - 4.), px(sh - 3.)));
                        if on > 0.5 || is_peak {
                            // Light spill around a lit segment.
                            let glow = Bounds::new(p(sx - 3., sy - 3.), size(px(cw + 2.), px(sh + 3.)));
                            window.paint_quad(fill(glow, Hsla { a: 0.07 + 0.08 * self.pulse, ..color }).corner_radii(px(4.)));
                        }
                        let color = if is_peak { Hsla { l: 0.75, ..color } } else { color };
                        window.paint_quad(fill(seg, Hsla { a: alpha, ..color }).corner_radii(px(2.)));
                    }
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
                // Pulse ring: a spectrum crown, three arcs turning on bass / mids / highs,
                // satellites with trails, a shockwave on every kick, a flash on the snare.
                let (cx, cy) = (w / 2., h / 2.);
                let m = w.min(h);
                let r0 = m * 0.17 * (1. + 0.12 * self.pulse);
                let len = m * 0.24;
                let n = 96;
                for i in 0..n {
                    // Mirrored: low notes at the top, high notes at the bottom, on both sides.
                    let half = i as f32 / n as f32 * 2.;
                    let x = if half <= 1. { half } else { 2. - half };
                    let lv = Viz::at(&self.levels, x);
                    let (s, c) = (i as f32 / n as f32 * TAU - PI / 2.).sin_cos();
                    let r1 = r0 + 3. + lv * len;
                    line(window, &[(cx + c * (r0 + 3.), cy + s * (r0 + 3.)), (cx + c * r1, cy + s * r1)], (r0 * TAU / n as f32 * 0.55).max(2.), self.color(x, 0.58, 0.9));
                    if x > 0.7 && self.hat > 0.2 {
                        circle(window, cx + c * (r1 + 4.), cy + s * (r1 + 4.), 1.5, self.color(x, 0.9, self.hat)); // hi-hat sparkle
                    }
                }
                let ring = |r: f32, from: f32, span: f32| -> Vec<(f32, f32)> {
                    (0..=48).map(|k| from + span * k as f32 / 48.).map(|a| (cx + a.cos() * r, cy + a.sin() * r)).collect()
                };
                line(window, &ring(r0, 0., TAU), 2. + 3. * self.snare, self.color(0., 0.75, 0.45 + 0.55 * self.pulse.max(self.snare)));
                // Arcs inside the ring: bass, mids, highs; length follows the level.
                let parts = [mean(&self.levels[0..4]), mean(&self.levels[5..11]), mean(&self.levels[11..16])];
                for (k, lv) in parts.iter().enumerate() {
                    let r = r0 * (0.45 + 0.17 * k as f32);
                    let dir = if k % 2 == 0 { 1. } else { -1. };
                    let from = dir * self.time * (0.15 + 0.08 * k as f32) * TAU;
                    let span = 0.25 * PI + lv * 0.7 * PI;
                    for half in [0., PI] {
                        line(window, &ring(r, from + half, span), 3., self.color(0.3 * k as f32, 0.6, 0.35 + 0.6 * lv));
                    }
                }
                // Satellites: orbit at the beat's pace, trailing fading copies.
                let spin = if self.bpm > 0. { self.bpm / 60. * 0.25 } else { 0.3 } * TAU;
                for k in 0..3 {
                    let r = r0 + len * 0.9 + k as f32 * 14.;
                    let a = self.time * spin * (1. + 0.2 * k as f32) + k as f32 * TAU / 3.;
                    for j in (0..14).rev() {
                        let aj = a - j as f32 * 0.05;
                        let fade = (-(j as f32) * 0.25).exp();
                        circle(window, cx + aj.cos() * r, cy + aj.sin() * r, (4. - j as f32 * 0.2).max(1.), self.color(0.5 + 0.2 * k as f32, 0.7, 0.9 * fade));
                    }
                }
                // Shockwaves.
                for age in &self.shocks {
                    let t = age / SHOCK_LIFE;
                    let r = r0 + t * m * 0.6;
                    line(window, &ring(r, 0., TAU), (4. * (1. - t)).max(0.5), self.color(0.1, 0.7, 0.6 * (1. - t) * (1. - t)));
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
            Style::Horizon => self.paint_horizon(w, h, &p, &line, window),
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
        // A drop flashes the whole picture.
        if self.drop > 0.02 && self.style != Style::Off {
            window.paint_quad(fill(b, self.color(0.2, 0.85, 0.22 * self.drop)));
        }
    }

    /// Synthwave: striped sun over a neon grid racing at the tempo, with mountains on both sides
    /// built from the last few seconds of the spectrum (low notes by the road, highs outside).
    fn paint_horizon(&self, w: f32, h: f32, p: &dyn Fn(f32, f32) -> gpui::Point<Pixels>, line: &dyn Fn(&mut Window, &[(f32, f32)], f32, Hsla), window: &mut Window) {
        let hy = h * 0.55;
        let (pink, cyan) = (hsla((self.hue + 0.9).fract(), 0.9, 0.6, 1.), hsla((self.hue + 0.5).fract(), 0.9, 0.6, 1.));
        // Sky and floor.
        let sky = linear_gradient(180., linear_color_stop(hsla((self.hue + 0.7).fract(), 0.6, 0.04, 1.), 0.), linear_color_stop(hsla((self.hue + 0.85).fract(), 0.7, 0.22, 1.), 1.));
        window.paint_quad(fill(Bounds::new(p(0., 0.), size(px(w), px(hy))), sky));
        window.paint_quad(fill(Bounds::new(p(0., hy), size(px(w), px(h - hy))), hsla((self.hue + 0.75).fract(), 0.5, 0.03, 1.)));
        for s in self.stars.iter().take(90) {
            let (x, y) = ((s.x + 1.) / 2. * w, (s.y + 1.) / 2. * hy * 0.8);
            let r = 0.6 + (1. - s.z) * 1.2;
            window.paint_quad(fill(Bounds::new(p(x - r, y - r), size(px(r * 2.), px(r * 2.))), hsla(0., 0., 1., 0.3 + 0.5 * (1. - s.z))).corner_radii(px(r)));
        }
        // Sun: horizontal strips, gaps widening towards the bottom, yellow to pink.
        let r = w.min(h) * 0.2 * (1. + 0.05 * self.pulse);
        let (sx, sy) = (w / 2., hy - r * 0.3);
        let mut y = sy - r;
        while y < hy.min(sy + r) {
            let below = ((y - sy) / r).max(0.);
            let gap = if below > 0. && ((y - sy) % 14.) < 2. + below * 8. { true } else { false };
            if !gap {
                let half = (r * r - (y - sy) * (y - sy)).max(0.).sqrt();
                let t = ((y - (sy - r)) / (2. * r)).clamp(0., 1.);
                let c = hsla((0.13 - 0.21 * t).rem_euclid(1.), 1., 0.6, 1.);
                window.paint_quad(fill(Bounds::new(p(sx - half, y), size(px(half * 2.), px(2.))), c));
            }
            y += 2.;
        }
        // Floor grid: lines converge on the vanishing point; cross lines race towards the viewer.
        let depth = h - hy;
        let proj = |z: f32| hy + depth / z; // z = 1 at the bottom edge
        for k in -14i32..=14 {
            let xb = w / 2. + k as f32 * w * 0.09;
            line(window, &[(w / 2. + k as f32 * w * 0.002, hy), (xb, h)], 1.2, Hsla { a: 0.5, ..pink });
        }
        let speed = if self.bpm > 0. { self.bpm / 60. } else { 1.5 };
        let phase = (self.time * speed).fract();
        for n in 0..24 {
            let z = 1. + (n as f32 - phase) * 0.9;
            if z < 1. {
                continue;
            }
            let y = proj(z);
            line(window, &[(0., y), (w, y)], 1.2, Hsla { a: (0.7 / z).min(0.7) + 0.2 * self.pulse, ..pink });
        }
        // Mountains, far to near so nearer ridges cover farther ones.
        let rows = self.history.len();
        for k in (0..rows).rev() {
            let z = 3. + (k as f32 + self.scroll) * 0.9;
            let (base, scale) = (proj(z), 3. / z);
            let fade = 1. - k as f32 / HISTORY as f32;
            let pts: Vec<(f32, f32)> = (0..=64)
                .map(|i| {
                    let wx = -1.7 + 3.4 * i as f32 / 64.; // world x
                    let side = ((wx.abs() - 0.3) / 0.5).clamp(0., 1.); // flat road in the middle
                    let lv = Viz::at(&self.history[k], ((wx.abs() - 0.3) / 1.4).clamp(0., 1.));
                    (w / 2. + wx * w * 0.5 * scale, base - lv * side * h * 0.42 * scale)
                })
                .collect();
            let mut shape = PathBuilder::fill();
            shape.move_to(p(pts[0].0, base));
            for &(x, y) in &pts {
                shape.line_to(p(x, y));
            }
            shape.line_to(p(pts[pts.len() - 1].0, base));
            shape.close();
            if let Ok(shape) = shape.build() {
                window.paint_path(shape, hsla((self.hue + 0.75).fract(), 0.5, 0.03, 1.));
            }
            line(window, &pts, 1.5, Hsla { a: 0.15 + 0.75 * fade, ..cyan });
        }
    }
}

fn mean(v: &[f32]) -> f32 {
    v.iter().sum::<f32>() / v.len() as f32
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
