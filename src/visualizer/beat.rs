//! Beat analysis on the visualizer's band levels: kick, snare and hi-hat onsets, tempo and drops.
//! Ported from CurioCrafter/music-visualizer's analyzer.rs (MIT, © CurioCrafter), adapted to our
//! 16 bands arriving ~18×/s instead of a 96-band FFT at 60 Hz.

use std::collections::VecDeque;

use super::BANDS;

/// One onset detector: a rise in level (flux) well above its recent average and spread.
pub struct Onset {
    prev: f32,
    mean: f32,
    var: f32,
    since: f32,
    refractory: f32,
    decay: f32,
    /// 1 on a hit, decays to 0.
    pub env: f32,
}

impl Onset {
    fn new(refractory: f32, decay: f32) -> Self {
        Onset { prev: 0., mean: 0., var: 0., since: 10., refractory, decay, env: 0. }
    }

    /// Feed this update's level; true on a hit.
    fn feed(&mut self, level: f32, dt: f32) -> bool {
        let flux = (level - self.prev).max(0.);
        self.prev = level;
        self.since += dt;
        let hit = self.since >= self.refractory && flux > self.mean + 1.6 * self.var.sqrt() + 0.04;
        // Running mean/variance of the flux over ~1 s.
        let a = 1. - (-dt).exp();
        let d = flux - self.mean;
        self.mean += a * d;
        self.var = (1. - a) * (self.var + a * d * d);
        if hit {
            self.since = 0.;
            self.env = 1.;
        }
        hit
    }

    fn fade(&mut self, dt: f32) {
        self.env *= (-self.decay * dt).exp();
    }
}

pub struct Beats {
    /// Bass drum: 40–130 Hz.
    pub kick: Onset,
    /// Snare: crack (2–5 kHz) plus body (200–300 Hz).
    pub snare: Onset,
    /// Hi-hat: 7 kHz and up.
    pub hat: Onset,
    /// Tempo, 70–180 (0 = not known yet).
    pub bpm: f32,
    /// 1 when the bass comes back in after a break, decays to 0.
    pub drop: f32,
    kicks: VecDeque<f32>,
    clock: f32,
    quiet_for: f32,
    since_drop: f32,
}

fn mean(v: &[f32]) -> f32 {
    v.iter().sum::<f32>() / v.len() as f32
}

impl Beats {
    pub fn new() -> Self {
        Beats { kick: Onset::new(0.12, 8.), snare: Onset::new(0.06, 7.), hat: Onset::new(0.05, 16.), bpm: 0., drop: 0., kicks: VecDeque::new(), clock: 0., quiet_for: 0., since_drop: 100. }
    }

    /// New band levels (0..=1), `dt` seconds after the previous ones. Returns true on a kick.
    pub fn feed(&mut self, b: &[f32; BANDS], dt: f32) -> bool {
        self.clock += dt;
        let bass = mean(&b[0..4]);
        let kick = self.kick.feed(bass, dt);
        self.snare.feed(0.6 * mean(&b[10..13]) + 0.4 * mean(&b[4..6]), dt);
        self.hat.feed(mean(&b[13..16]), dt);
        if kick {
            self.kicks.push_back(self.clock);
            if self.kicks.len() > 13 {
                self.kicks.pop_front();
            }
            if let Some(bpm) = tempo(&self.kicks) {
                self.bpm = if self.bpm == 0. { bpm } else { self.bpm + 0.3 * (bpm - self.bpm) };
            }
        }
        // Drop: heavy bass after at least 1.5 s without, at most every 8 s.
        self.since_drop += dt;
        if bass < 0.25 {
            self.quiet_for += dt;
        } else {
            if bass > 0.5 && self.quiet_for >= 1.5 && self.since_drop >= 8. {
                self.drop = 1.;
                self.since_drop = 0.;
            }
            self.quiet_for = 0.;
        }
        kick
    }

    /// Let the envelopes fade (every frame).
    pub fn fade(&mut self, dt: f32) {
        self.kick.fade(dt);
        self.snare.fade(dt);
        self.hat.fade(dt);
        self.drop *= (-1.2 * dt).exp();
    }
}

/// BPM from kick times: median interval, folded into 70–180.
fn tempo(kicks: &VecDeque<f32>) -> Option<f32> {
    let mut iv: Vec<f32> = kicks.iter().zip(kicks.iter().skip(1)).map(|(a, b)| b - a).filter(|d| (0.2..2.).contains(d)).collect();
    if iv.len() < 3 {
        return None;
    }
    iv.sort_by(f32::total_cmp);
    let mut bpm = 60. / iv[iv.len() / 2];
    while bpm < 70. {
        bpm *= 2.;
    }
    while bpm > 180. {
        bpm /= 2.;
    }
    Some(bpm)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of updates every 55 ms; bass `kick_level` on every beat at `bpm`, else `floor`.
    fn play(b: &mut Beats, secs: f32, bpm: f32, kick_level: f32, floor: f32) -> usize {
        let (dt, mut t, mut hits) = (0.055, 0., 0);
        let period = 60. / bpm;
        while t < secs {
            let on = (t % period) < dt;
            let mut bands = [0.2; BANDS];
            bands[..4].fill(if on { kick_level } else { floor });
            hits += b.feed(&bands, dt) as usize;
            t += dt;
        }
        hits
    }

    #[test]
    fn kicks_and_tempo() {
        let mut b = Beats::new();
        let hits = play(&mut b, 10., 120., 0.9, 0.3);
        assert!((17..=21).contains(&hits), "about one hit per beat: {hits}");
        assert!((b.bpm - 120.).abs() < 8., "bpm {}", b.bpm);
        let mut slow = Beats::new();
        play(&mut slow, 12., 60., 0.9, 0.3);
        assert!((slow.bpm - 120.).abs() < 10., "60 bpm folds to 120: {}", slow.bpm);
    }

    #[test]
    fn steady_level_is_not_a_beat() {
        let mut b = Beats::new();
        assert_eq!(play(&mut b, 5., 120., 0.5, 0.5), 1, "only the first rise from silence");
    }

    #[test]
    fn drop_after_a_break() {
        let mut b = Beats::new();
        play(&mut b, 3., 120., 0.1, 0.1); // break: no bass
        assert_eq!(b.drop, 0.);
        play(&mut b, 0.2, 120., 0.9, 0.9);
        assert_eq!(b.drop, 1., "bass returns -> drop");
    }
}
