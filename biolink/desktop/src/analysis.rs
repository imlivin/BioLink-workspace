//! Turns a session's IMU series into reps, sets, tempo, range of motion and quality scores.
//!
//! Pipeline
//! 1. Complementary filter (gyro + accel) tracks the gravity direction in the sensor frame.
//! 2. Candidate "rep signals" are derived (see [`Mode`]): forearm tilt, arm rotation from the
//!    gyro, yaw about vertical, vertical or linear travel. The one that repeats most regularly
//!    (autocorrelation) is used, with a preference for the exercise's usual signal. The
//!    autocorrelation peak also gives the rep period.
//! 3. The signal is low-pass filtered and oriented so the rest position is low. Each movement
//!    peak that stands out at least 40% as much as a typical rep, and is at least about half a
//!    period from the previous one, is a rep.
//! 4. Each rep is split into out / hold / back phases and scored.

use crate::dsp::{self, V3};
use crate::exercises::{Exercise, Mode};
use crate::model::ImuSeries;

const G: f64 = 9.80665;

#[derive(Clone, Debug)]
pub struct Rep {
    pub index: usize,
    pub set: usize,
    pub start_t: f64,
    pub peak_t: f64,
    pub end_t: f64,
    pub duration_s: f64,
    /// Moving away from the start position.
    pub out_s: f64,
    /// Time held at the far end of the movement.
    pub hold_s: f64,
    /// Returning to the start position (0 for half-cycle exercises).
    pub back_s: f64,
    pub concentric_s: Option<f64>,
    pub eccentric_s: Option<f64>,
    /// Range of motion in the mode's unit.
    pub rom: f64,
    /// Peak speed in unit/s.
    pub peak_speed: f64,
    /// Correlation with the session's median rep shape (1 = identical).
    pub similarity: f64,

    /// Share of the rep that fell in a gap in the data (0..1).
    pub gap_frac: f64,
    /// Cut off by the start of the recording: counted, but left out of timing and form scores.
    pub partial: bool,
    // sample indices
    pub i_start: usize,
    pub i_peak: usize,
    pub i_end: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SetStats {
    pub index: usize,
    pub reps: usize,
    pub start_t: f64,
    pub end_t: f64,
    pub rest_before_s: Option<f64>,
    pub rom_mean: f64,
    pub rom_cv: f64,
    pub duration_mean: f64,
    pub duration_cv: f64,
    /// Mean time between movement peaks (s), i.e. the pace of the set.
    pub cycle_mean: Option<f64>,
    /// Variation of the time between movement peaks: how steady the rhythm is.
    pub cycle_cv: Option<f64>,
    /// Drop in peak speed from the first two to the last two reps (%).
    pub speed_loss_pct: Option<f64>,
    /// Drop in range of motion from the first two to the last two reps (%).
    pub rom_drop_pct: Option<f64>,
    pub tut_s: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Scores {
    pub consistency: Option<f64>,
    pub form: Option<f64>,
    pub tempo: Option<f64>,
    pub overall: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub reps: usize,
    pub sets: usize,
    /// Unit of rom_mean and peak_speed_mean.
    pub unit: &'static str,
    pub session_s: f64,
    pub active_s: f64,
    pub tut_s: f64,
    pub rom_mean: f64,
    pub duration_mean: f64,
    pub concentric_mean: Option<f64>,
    pub eccentric_mean: Option<f64>,
    pub hold_mean: f64,
    pub peak_speed_mean: f64,
    pub rest_mean_s: Option<f64>,
    pub scores: Scores,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub mode: Mode,
    pub unit: &'static str,
    pub t: Vec<f64>,
    /// Rep signal, oriented so the start position is low.
    pub signal: Vec<f64>,
    /// Typical time from one rep to the next (s), from the signal's autocorrelation.
    pub period_s: f64,
    /// Signals that were tried: (mode, regularity 0..1, period s).
    pub candidates: Vec<(Mode, f64, f64)>,
    pub reps: Vec<Rep>,
    pub sets: Vec<SetStats>,
    pub summary: Summary,
}

impl Analysis {
    /// Starting point for a session that is being recorded: pass it as `prev` to [`analyze`]
    /// so counting waits for enough data.
    pub fn recording_start() -> Self {
        Analysis::empty(Mode::Tilt, "")
    }

    fn empty(mode: Mode, note: &str) -> Self {
        Analysis {
            mode,
            unit: mode.unit(),
            t: vec![],
            signal: vec![],
            period_s: 0.0,
            candidates: vec![],
            reps: vec![],
            sets: vec![],
            summary: Summary {
                notes: vec![note.to_string()],
                ..Default::default()
            },
        }
    }
}

/// Analyse a session. `prev` is the previous result for the same session while it is still
/// being recorded: reps already counted then are kept, so the live rep count never goes down.
pub fn analyze(
    series: &ImuSeries,
    ex: &Exercise,
    session_s: Option<f64>,
    prev: Option<&Analysis>,
) -> Analysis {
    let fs = series.hz;
    let n = series.t.len();
    if n < (fs * 3.0) as usize {
        let mut a = Analysis::empty(
            ex.mode,
            "Not enough sensor data yet (need at least 3 seconds).",
        );
        a.t = series.t.clone();
        a.summary.session_s = session_s.unwrap_or(series.duration());
        return a;
    }

    let grav = gravity_track(series, fs);

    // 1. Pick the signal that repeats most clearly. The exercise's usual signal is preferred;
    //    another one is used only when it is clearly more regular (e.g. lunges barely tilt the
    //    forearm but swing it). Once reps are counted the choice is kept.
    let min_period = 0.8 * ex.min_rep_s;
    let max_period = (1.5 * ex.target_rep_s.1).clamp(4.0, 12.0);
    // While recording, the signal and rep period are chosen once there is enough data to
    // judge them, then kept so earlier reps stay valid.
    let locked = prev.filter(|p| !p.reps.is_empty());
    let warming_up = prev.is_some() && locked.is_none() && series.duration() < LIVE_WARMUP_S;
    let modes: Vec<Mode> = match locked {
        Some(p) => vec![p.mode],
        None => std::iter::once(ex.mode)
            .chain(Mode::ALL.into_iter().filter(|m| *m != ex.mode))
            .collect(),
    };
    let mut candidates = Vec::new();
    let mut best: Option<(f64, Mode, Vec<f64>, f64)> = None;
    for m in modes {
        let raw = rep_signal(m, series, &grav, fs);
        let sig = dsp::lowpass(&raw, ex.lowpass_hz, fs);
        let (mut score, period) = periodicity(&sig, &series.valid, fs, min_period, max_period);
        // a regular but tiny movement (sensor noise, small wobble) can't carry the reps
        let swing = dsp::percentile(&sig, 95.0) - dsp::percentile(&sig, 5.0);
        if swing < 1.5 * m.min_rom() {
            score *= 0.5;
        }
        candidates.push((m, score, period));
        let rank = score + if m == ex.mode { PREFER_OWN_SIGNAL } else { 0.0 };
        if best.as_ref().is_none_or(|b| rank > b.0) {
            best = Some((rank, m, sig, period));
        }
    }
    let Some((_, mode, mut sig, period)) = best else {
        return Analysis::empty(ex.mode, "No usable signal.");
    };
    let period = match locked {
        Some(p) => p.period_s,
        None if period > 0.0 => period,
        None => 0.5 * (ex.target_rep_s.0 + ex.target_rep_s.1).min(6.0),
    };

    // 2. Orient so the rest position is low and each rep is one peak.
    let flip = match prev.filter(|p| p.mode == mode && !p.signal.is_empty()) {
        // keep the direction used before
        Some(p) => {
            let k = p.signal.len().min(sig.len());
            dsp::pearson(&p.signal[..k], &sig[..k]) < 0.0
        }
        // the rest position is where the signal spends most time, so the median sits near it
        None => {
            let med = dsp::median(&sig);
            let hi = dsp::percentile(&sig, 95.0);
            let lo = dsp::percentile(&sig, 5.0);
            med - lo > hi - med
        }
    };
    if flip {
        sig.iter_mut().for_each(|v| *v = -*v);
    }

    let still = stillness(series, &grav, &sig, mode, fs);
    let mut reps = if warming_up {
        vec![]
    } else {
        find_reps(&sig, period, fs, ex, mode, &still)
    };

    let fine_speed: Vec<f64> = dsp::derivative(&dsp::lowpass(&sig, 4.0, fs), 1.0 / fs)
        .iter()
        .map(|v| v.abs())
        .collect();
    for r in reps.iter_mut() {
        let seg = r.i_start..=r.i_end;
        r.gap_frac = series.valid[seg.clone()].iter().filter(|v| !**v).count() as f64
            / (r.i_end - r.i_start + 1) as f64;
        r.peak_speed = fine_speed[seg].iter().cloned().fold(0.0, f64::max);
        r.start_t = series.t[r.i_start];
        r.peak_t = series.t[r.i_peak];
        r.end_t = series.t[r.i_end];
    }

    // Drop movements that are largely missing from the data (gaps are filled as "still",
    // so they can't create peaks themselves).
    let before = reps.len();
    reps.retain(|r| r.gap_frac <= 0.4);
    let rejected = Rejected { in_gap: before - reps.len() };
    // While recording, start showing reps once at least two are found, so a single
    // doubtful one isn't shown and then taken back.
    if prev.is_some() && locked.is_none() && reps.len() < 2 {
        reps.clear();
    }

    // 3. While recording, keep every rep counted before. Only the newest one may still be
    //    updated (its end wasn't recorded yet), and new reps are added after it.
    if let Some(p) = prev.filter(|p| p.mode == mode) {
        let mut kept: Vec<Rep> = p.reps.iter().filter(|r| r.i_end < n).cloned().collect();
        if let Some(last) = kept.last().cloned() {
            let same = |r: &Rep| (r.peak_t - last.peak_t).abs() < 0.4 * period;
            if let Some(updated) = reps.iter().find(|r| same(r)) {
                *kept.last_mut().unwrap() = updated.clone();
            }
            let after = kept.last().map(|r| r.peak_t).unwrap_or(f64::MIN);
            kept.extend(
                reps.into_iter()
                    .filter(|r| r.peak_t > after + 0.4 * period && !same(r)),
            );
            reps = kept;
        }
    }

    // shape similarity against the median rep
    if reps.len() >= 2 {
        let shapes: Vec<Vec<f64>> = reps
            .iter()
            .map(|r| {
                let s = dsp::resample(&sig[r.i_start..=r.i_end], 50);
                let (lo, hi) = s
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
                s.iter().map(|v| (v - lo) / (hi - lo).max(1e-9)).collect()
            })
            .collect();
        // the typical rep, from reps that weren't cut off by the start of the recording
        let whole: Vec<&Vec<f64>> = shapes
            .iter()
            .zip(&reps)
            .filter(|(_, r)| !r.partial)
            .map(|(s, _)| s)
            .collect();
        let template: Vec<f64> = (0..50)
            .map(|k| dsp::median(&whole.iter().map(|s| s[k]).collect::<Vec<_>>()))
            .collect();
        for (r, s) in reps.iter_mut().zip(&shapes) {
            r.similarity = dsp::pearson(s, &template);
        }
    } else {
        reps.iter_mut().for_each(|r| r.similarity = 1.0);
    }

    let sets = group_sets(&mut reps);
    let mut summary = summarize(
        &reps,
        &sets,
        ex,
        session_s.unwrap_or(series.duration()),
        series,
        rejected,
    );
    summary.unit = mode.unit();
    if prev.is_some() && locked.is_none() && reps.is_empty() {
        summary.notes.insert(
            0,
            "Counting starts once two reps are recognised (after about 8 seconds).".into(),
        );
    }
    if mode != ex.mode && !reps.is_empty() {
        summary.notes.push(format!(
            "Tracked by {} instead of {}: it repeats more clearly in this recording.",
            mode.label().to_lowercase(),
            ex.mode.label().to_lowercase()
        ));
    }

    Analysis {
        mode,
        unit: mode.unit(),
        t: series.t.clone(),
        signal: sig,
        period_s: period,
        candidates,
        reps,
        sets,
        summary,
    }
}

/// While recording, reps are counted once this much data is in (s): enough to pick the
/// signal and rep period reliably. Before that the count would jump around.
const LIVE_WARMUP_S: f64 = 8.0;

/// Bonus for the exercise's own tracking signal when choosing between signals.
const PREFER_OWN_SIGNAL: f64 = 0.15;

fn rep_signal(mode: Mode, s: &ImuSeries, grav: &[V3], fs: f64) -> Vec<f64> {
    match mode {
        Mode::Tilt => tilt_signal(grav),
        Mode::Rotation => yaw_signal(s, grav, fs),
        Mode::Vertical => vertical_signal(s, grav, fs),
        Mode::Linear => linear_signal(s, grav, fs),
        Mode::Swing => swing_signal(s, fs),
    }
}

/// How regularly `x` repeats: the highest autocorrelation peak for a lag between `min_s` and
/// `max_s` seconds (1 = perfectly periodic) and that lag (the rep period).
fn periodicity(x: &[f64], valid: &[bool], fs: f64, min_s: f64, max_s: f64) -> (f64, f64) {
    let step = ((fs / 10.0).round() as usize).max(1);
    let hz = fs / step as f64;
    let mut y: Vec<f64> = x.iter().step_by(step).copied().collect();
    let m = dsp::mean(&y);
    for (k, v) in y.iter_mut().enumerate() {
        *v = if valid.get(k * step).copied().unwrap_or(true) { *v - m } else { 0.0 };
    }
    let n = y.len();
    let lmin = ((min_s * hz).ceil() as usize).max(1);
    let lmax = ((max_s * hz) as usize).min(n / 2);
    let e: f64 = y.iter().map(|v| v * v).sum();
    if lmax <= lmin + 1 || e <= 0.0 {
        return (0.0, 0.0);
    }
    let r: Vec<f64> = (0..=lmax + 1)
        .map(|l| (0..n - l).map(|i| y[i] * y[i + l]).sum::<f64>() / e)
        .collect();
    let peaks: Vec<usize> = (lmin..=lmax)
        .filter(|&l| r[l] > r[l - 1] && r[l] >= r[l + 1] && r[l] > 0.0)
        .collect();
    let top = peaks.iter().map(|&l| r[l]).fold(0.0, f64::max);
    // A signal with period P also correlates at 2P, 3P...; take the shortest lag that is
    // nearly as strong as the best one.
    peaks
        .iter()
        .find(|&&l| r[l] >= 0.65 * top)
        .map(|&l| (r[l], l as f64 / hz))
        .unwrap_or((0.0, 0.0))
}

/// true where the wrist is still for at least 0.3 s: smoothed movement acceleration under
/// 0.05 g (travel modes) or angular speed under 15°/s (angle modes).
fn stillness(s: &ImuSeries, grav: &[V3], raw: &[f64], mode: Mode, fs: f64) -> Vec<bool> {
    let (activity, thr): (Vec<f64>, f64) = match mode {
        Mode::Vertical => (s.acc.iter().zip(grav).map(|(a, g)| dsp::dot(*a, *g) - 1.0).collect(), 0.05),
        Mode::Linear => (s.acc.iter().map(|a| dsp::norm(*a) - 1.0).collect(), 0.05),
        Mode::Tilt | Mode::Rotation | Mode::Swing => (dsp::derivative(raw, 1.0 / fs), 15.0),
    };
    let act: Vec<f64> = dsp::lowpass(&activity, 3.0, fs).iter().map(|v| v.abs()).collect();
    let run = ((0.3 * fs) as usize).max(1);
    let quiet: Vec<bool> = act.iter().map(|v| *v < thr).collect();
    let mut out = vec![false; quiet.len()];
    let mut i = 0;
    while i < quiet.len() {
        if quiet[i] {
            let j = (i..quiet.len()).find(|&k| !quiet[k]).unwrap_or(quiet.len());
            if j - i >= run {
                out[i..j].iter_mut().for_each(|v| *v = true);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Gravity ("up") direction in the sensor frame, from a complementary filter (gyro + accel).
pub fn gravity_track(s: &ImuSeries, fs: f64) -> Vec<V3> {
    let dt = 1.0 / fs;
    let beta = (dt / 1.0).min(0.2); // ~1 s correction time constant
    let init_n = (fs as usize / 2).clamp(1, s.acc.len());
    let mut g = dsp::normalize(
        s.acc[..init_n]
            .iter()
            .fold([0.0; 3], |a, b| dsp::add(a, *b)),
    );
    let mut out = Vec::with_capacity(s.acc.len());
    for (a, w) in s.acc.iter().zip(&s.gyr) {
        let w_rad = dsp::scale(*w, std::f64::consts::PI / 180.0);
        g = dsp::normalize(dsp::sub(g, dsp::scale(dsp::cross(w_rad, g), dt)));
        let an = dsp::norm(*a);
        if (0.8..1.2).contains(&an) {
            g = dsp::normalize(dsp::add(
                dsp::scale(g, 1.0 - beta),
                dsp::scale(*a, beta / an),
            ));
        }
        out.push(g);
    }
    out
}

/// Forearm angle in the plane of largest movement, in degrees.
fn tilt_signal(grav: &[V3]) -> Vec<f64> {
    let (_, vecs, mean) = dsp::pca3(grav);
    let m = dsp::normalize(mean);
    let e1 = dsp::normalize(dsp::sub(vecs[0], dsp::scale(m, dsp::dot(vecs[0], m))));
    let mut out: Vec<f64> = grav
        .iter()
        .map(|g| dsp::dot(*g, e1).atan2(dsp::dot(*g, m)).to_degrees())
        .collect();
    // unwrap
    for i in 1..out.len() {
        let d = out[i] - out[i - 1];
        if d > 180.0 {
            for v in &mut out[i..] {
                *v -= 360.0;
            }
        } else if d < -180.0 {
            for v in &mut out[i..] {
                *v += 360.0;
            }
        }
    }
    out
}

/// Rotation about the vertical axis, degrees (drift removed).
fn yaw_signal(s: &ImuSeries, grav: &[V3], fs: f64) -> Vec<f64> {
    let rate: Vec<f64> = s
        .gyr
        .iter()
        .zip(grav)
        .map(|(w, g)| dsp::dot(*w, *g))
        .collect();
    dsp::highpass(&dsp::integrate(&rate, 1.0 / fs), 0.05, fs)
}

const HP: f64 = 0.08;

fn double_integrate_cm(acc_ms2: &[f64], fs: f64) -> Vec<f64> {
    let dt = 1.0 / fs;
    let a = dsp::highpass(acc_ms2, HP, fs);
    let v = dsp::highpass(&dsp::integrate(&a, dt), HP, fs);
    dsp::highpass(&dsp::integrate(&v, dt), HP, fs)
        .iter()
        .map(|p| p * 100.0)
        .collect()
}

/// Estimated vertical travel of the wrist, cm.
fn vertical_signal(s: &ImuSeries, grav: &[V3], fs: f64) -> Vec<f64> {
    let av: Vec<f64> = s
        .acc
        .iter()
        .zip(grav)
        .map(|(a, g)| (dsp::dot(*a, *g) - 1.0) * G)
        .collect();
    double_integrate_cm(&av, fs)
}

/// Estimated travel along the main movement direction, cm.
fn linear_signal(s: &ImuSeries, grav: &[V3], fs: f64) -> Vec<f64> {
    let lin: Vec<V3> = s
        .acc
        .iter()
        .zip(grav)
        .map(|(a, g)| dsp::scale(dsp::sub(*a, *g), G))
        .collect();
    let comp = |k: usize| dsp::bandpass(&lin.iter().map(|v| v[k]).collect::<Vec<_>>(), HP, 5.0, fs);
    let (x, y, z) = (comp(0), comp(1), comp(2));
    let pts: Vec<V3> = (0..lin.len()).map(|i| [x[i], y[i], z[i]]).collect();
    let (_, vecs, _) = dsp::pca3(&pts);
    let proj: Vec<f64> = pts.iter().map(|p| dsp::dot(*p, vecs[0])).collect();
    double_integrate_cm(&proj, fs)
}

/// Forearm rotation about the main axis it turns around, from the gyroscope (degrees).
fn swing_signal(s: &ImuSeries, fs: f64) -> Vec<f64> {
    let comp = |k: usize| dsp::bandpass(&s.gyr.iter().map(|w| w[k]).collect::<Vec<_>>(), 0.1, 3.0, fs);
    let (x, y, z) = (comp(0), comp(1), comp(2));
    let pts: Vec<V3> = (0..x.len()).map(|i| [x[i], y[i], z[i]]).collect();
    let (_, vecs, _) = dsp::pca3(&pts);
    let rate: Vec<f64> = pts.iter().map(|p| dsp::dot(*p, vecs[0])).collect();
    dsp::highpass(&dsp::integrate(&rate, 1.0 / fs), 0.1, fs)
}

fn blank_rep(i_start: usize, i_peak: usize, i_end: usize) -> Rep {
    Rep {
        index: 0,
        set: 0,
        start_t: 0.0,
        peak_t: 0.0,
        end_t: 0.0,
        duration_s: 0.0,
        out_s: 0.0,
        hold_s: 0.0,
        back_s: 0.0,
        concentric_s: None,
        eccentric_s: None,
        rom: 0.0,
        peak_speed: 0.0,
        similarity: 1.0,
        gap_frac: 0.0,
        partial: false,
        i_start,
        i_peak,
        i_end,
    }
}

/// Each movement peak is one rep. Peaks must be at least ~half a rep period apart and stand
/// out at least 40% as much as a typical rep, so small wobbles between reps don't count.
fn find_reps(sig: &[f64], period: f64, fs: f64, ex: &Exercise, mode: Mode, still: &[bool]) -> Vec<Rep> {
    let n = sig.len();
    let min_dist = ((0.45 * period).max(0.8 * ex.min_rep_s) * fs) as usize;
    let floor = 0.5 * mode.min_rom();
    let cands = dsp::find_peaks(sig, floor, min_dist.max(1));
    if cands.is_empty() {
        return vec![];
    }
    let proms: Vec<f64> = cands.iter().map(|&p| dsp::prominence(sig, p)).collect();
    let typical = dsp::percentile(&proms, 75.0);
    let min_rom = (0.4 * typical).max(mode.min_rom());
    let mut peaks: Vec<usize> = cands
        .iter()
        .zip(&proms)
        .filter(|(_, pr)| **pr >= min_rom)
        .map(|(p, _)| *p)
        .collect();
    // Data arrives up to a second after the start press, so the recording can begin
    // mid-rep. A drop right from the start that is as big as a typical rep is that first rep.
    let head = (0.3 * fs) as usize;
    let first = peaks.first().copied().unwrap_or(n - 1);
    if first > head {
        let v = dsp::argmin(&sig[..first]);
        let top = (0..=head.min(v)).fold(0, |b, i| if sig[i] > sig[b] { i } else { b });
        if v > top && sig[top] - sig[v] >= (0.6 * typical).max(mode.min_rom()) {
            peaks.insert(0, top);
        }
    }

    // a rep's start/end are searched within one period of its peak
    let window = ((1.0 * period).max(ex.min_rep_s) * fs) as usize;
    let mut reps = Vec::new();
    for (k, &p) in peaks.iter().enumerate() {
        let left = if k > 0 { peaks[k - 1].max(p.saturating_sub(window)) } else { p.saturating_sub(window) };
        let right = if k + 1 < peaks.len() { peaks[k + 1].min(p + window) } else { (p + window).min(n - 1) };
        if right <= p {
            continue;
        }
        let e0 = p + dsp::argmin(&sig[p..=right]);
        // a rep cut off by the start of the recording has no rising part
        let partial = p <= left;
        let s0 = if partial { p } else { left + dsp::argmin(&sig[left..p]) };
        let peak = sig[p];
        let te = sig[e0];
        let ts = if partial { te } else { sig[s0] };
        let rom = peak - 0.5 * (ts + te);
        if rom < min_rom {
            continue;
        }
        // movement starts/ends where 10% of the excursion is crossed...
        let mut ms = p;
        while ms > s0 && sig[ms] > ts + 0.1 * (peak - ts) {
            ms -= 1;
        }
        let mut me = p;
        while me < e0 && sig[me] > te + 0.1 * (peak - te) {
            me += 1;
        }
        // ...or earlier, where the wrist stops near its rest position (integrated signals
        // drift while the wrist is still). Only stillness in the bottom third counts, since
        // steady mid-rep motion also reads as "still" to an accelerometer.
        if let Some(i) = (ms..p).rev().find(|&i| still[i] && sig[i] < ts + 0.35 * (peak - ts)) {
            ms = i + 1;
        }
        if let Some(i) = (p..=me).find(|&i| still[i] && sig[i] < te + 0.35 * (peak - te)) {
            me = i.saturating_sub(1).max(p);
        }
        // time at the top: within 10% of the peak
        let top = peak - 0.1 * rom;
        let mut ta = p;
        while ta > ms && sig[ta - 1] >= top {
            ta -= 1;
        }
        let mut tb = p;
        while tb < me && sig[tb + 1] >= top {
            tb += 1;
        }
        let mut r = blank_rep(ms, p, me);
        r.partial = partial;
        r.duration_s = (me - ms) as f64 / fs;
        if !partial && r.duration_s < 0.5 * ex.min_rep_s {
            continue;
        }
        r.out_s = (ta - ms) as f64 / fs;
        r.hold_s = (tb - ta) as f64 / fs;
        r.back_s = (me - tb) as f64 / fs;
        match ex.out_is_concentric {
            Some(true) => {
                r.concentric_s = Some(r.out_s);
                r.eccentric_s = Some(r.back_s);
            }
            Some(false) => {
                r.concentric_s = Some(r.back_s);
                r.eccentric_s = Some(r.out_s);
            }
            None => {}
        }
        r.rom = rom;
        reps.push(r);
    }
    reps
}

fn group_sets(reps: &mut [Rep]) -> Vec<SetStats> {
    if reps.is_empty() {
        return vec![];
    }
    let med_dur = dsp::median(&reps.iter().map(|r| r.duration_s).collect::<Vec<_>>());
    let gap_limit = (3.0 * med_dur).max(8.0);
    let mut set = 0;
    for i in 0..reps.len() {
        if i > 0 && reps[i].start_t - reps[i - 1].end_t > gap_limit {
            set += 1;
        }
        reps[i].set = set;
        reps[i].index = i + 1;
    }
    let mut out = Vec::new();
    for s in 0..=set {
        let all: Vec<&Rep> = reps.iter().filter(|r| r.set == s).collect();
        // a rep cut off by the start of the recording has no reliable timing or range
        let rs: Vec<&Rep> = all.iter().copied().filter(|r| !r.partial).collect();
        let rs = if rs.is_empty() { all.clone() } else { rs };
        let roms: Vec<f64> = rs.iter().map(|r| r.rom).collect();
        let durs: Vec<f64> = rs.iter().map(|r| r.duration_s).collect();
        // rhythm: time from one movement peak to the next
        let cycles: Vec<f64> = all.windows(2).map(|w| w[1].peak_t - w[0].peak_t).collect();
        let first_last = |f: &dyn Fn(&Rep) -> f64| -> Option<f64> {
            if rs.len() < 4 {
                return None;
            }
            let first = (f(rs[0]) + f(rs[1])) / 2.0;
            let last = (f(rs[rs.len() - 1]) + f(rs[rs.len() - 2])) / 2.0;
            (first > 1e-9).then(|| (first - last) / first * 100.0)
        };
        let start_t = all[0].start_t;
        let rest_before_s = out.last().map(|p: &SetStats| start_t - p.end_t);
        out.push(SetStats {
            index: s + 1,
            reps: all.len(),
            start_t,
            end_t: all[all.len() - 1].end_t,
            cycle_mean: (!cycles.is_empty()).then(|| dsp::mean(&cycles)),
            cycle_cv: (cycles.len() >= 2).then(|| dsp::robust_cv(&cycles)),
            rest_before_s,
            rom_mean: dsp::mean(&roms),
            rom_cv: dsp::robust_cv(&roms),
            duration_mean: dsp::mean(&durs),
            duration_cv: dsp::robust_cv(&durs),
            speed_loss_pct: first_last(&|r| r.peak_speed),
            rom_drop_pct: first_last(&|r| r.rom),
            tut_s: durs.iter().sum(),
        });
    }
    out
}

/// Candidate reps that were not counted.
#[derive(Clone, Copy, Debug, Default)]
struct Rejected {
    in_gap: usize,
}

fn summarize(
    reps: &[Rep],
    sets: &[SetStats],
    ex: &Exercise,
    session_s: f64,
    series: &ImuSeries,
    rejected: Rejected,
) -> Summary {
    let mut s = Summary {
        session_s,
        reps: reps.len(),
        sets: sets.len(),
        ..Default::default()
    };
    if series.gaps > 0 {
        let missing = series.valid.iter().filter(|v| !**v).count() as f64 / series.hz;
        s.notes.push(format!(
            "{} gap(s) in the sensor data ({missing:.1} s missing in total). Movement during gaps can't be analysed.",
            series.gaps
        ));
    }
    if rejected.in_gap > 0 {
        s.notes.push(format!(
            "{} rep(s) not counted because most of the data for them is missing.",
            rejected.in_gap
        ));
    }

    if series.spikes > 0 {
        s.notes.push(format!("{} single-sample glitch(es) in the sensor data were removed.", series.spikes));
    }
    if reps.is_empty() {
        s.notes.push(
            "No reps detected. Check the exercise selected in the app, or the movement may be too small for the wrist sensor."
                .into(),
        );
        return s;
    }
    let col = |f: &dyn Fn(&Rep) -> f64| reps.iter().map(f).collect::<Vec<f64>>();
    let opt_mean = |f: &dyn Fn(&Rep) -> Option<f64>| {
        let v: Vec<f64> = reps.iter().filter_map(f).collect();
        (!v.is_empty()).then(|| dsp::mean(&v))
    };
    s.active_s = sets.iter().map(|x| x.end_t - x.start_t).sum();
    s.tut_s = sets.iter().map(|x| x.tut_s).sum();
    s.rom_mean = dsp::mean(&col(&|r| r.rom));
    s.duration_mean = dsp::mean(&col(&|r| r.duration_s));
    s.hold_mean = dsp::mean(&col(&|r| r.hold_s));
    s.peak_speed_mean = dsp::mean(&col(&|r| r.peak_speed));
    s.concentric_mean = opt_mean(&|r| r.concentric_s);
    s.eccentric_mean = opt_mean(&|r| r.eccentric_s);
    let rests: Vec<f64> = sets.iter().filter_map(|x| x.rest_before_s).collect();
    s.rest_mean_s = (!rests.is_empty()).then(|| dsp::mean(&rests));

    // ---- scores (0..100) ----
    // reps cut off by the start of the recording are counted but not scored
    let full: Vec<&Rep> = reps.iter().filter(|r| !r.partial).collect();
    let n = full.len() as f64;
    let multi: Vec<&SetStats> = sets.iter().filter(|x| x.reps >= 3).collect();
    if !multi.is_empty() {
        let w: f64 = multi.iter().map(|x| x.reps as f64).sum();
        let cv_rom = multi.iter().map(|x| x.rom_cv * x.reps as f64).sum::<f64>() / w;
        // rhythm: steadiness of the time between movement peaks (rep durations as fallback)
        let cv_rhythm = multi
            .iter()
            .map(|x| x.cycle_cv.unwrap_or(x.duration_cv) * x.reps as f64)
            .sum::<f64>()
            / w;
        s.scores.consistency =
            Some((100.0 - 150.0 * cv_rom - 150.0 * cv_rhythm).clamp(0.0, 100.0));
        if cv_rom > 0.15 {
            s.notes.push(format!(
                "Range of motion varies a lot between reps ({:.0}% variation).",
                cv_rom * 100.0
            ));
        }
        if cv_rhythm > 0.2 {
            s.notes.push(format!(
                "Uneven rhythm: the time between reps varies {:.0}%.",
                cv_rhythm * 100.0
            ));
        }
    }
    if full.len() >= 3 {
        let sim = dsp::mean(&full.iter().map(|r| r.similarity).collect::<Vec<_>>());
        s.scores.form = Some(((sim - 0.6) / 0.4 * 100.0).clamp(0.0, 100.0));
        let odd: Vec<String> = full
            .iter()
            .filter(|r| r.similarity < 0.8)
            .map(|r| r.index.to_string())
            .collect();
        if !odd.is_empty() {
            s.notes.push(format!(
                "Rep(s) {} move differently from the rest (possible form break).",
                odd.join(", ")
            ));
        }
    }
    // tempo: full marks inside the target range, falling to 0 at half the lower limit or
    // 1.5x the upper limit
    let (lo, hi) = ex.target_rep_s;
    let mut tempo_pts = 0.0;
    let mut rushed_ecc = 0;
    let mut too_fast = 0;
    let mut too_slow = 0;
    for r in &full {
        let d = r.duration_s;
        let mut p = if d < lo {
            too_fast += 1;
            (1.0 - (lo - d) / (0.5 * lo)).max(0.0)
        } else if d > hi {
            too_slow += 1;
            (1.0 - (d - hi) / (0.5 * hi)).max(0.0)
        } else {
            1.0
        };
        // the wrist only roughly separates lifting from lowering, so this costs a quarter
        if let (Some(c), Some(e)) = (r.concentric_s, r.eccentric_s) {
            if e < 0.6 * c {
                rushed_ecc += 1;
                p *= 0.75;
            }
        }
        tempo_pts += p;
    }
    if n > 0.0 {
        s.scores.tempo = Some(tempo_pts / n * 100.0);
    }
    if too_fast as f64 > 0.3 * n {
        s.notes.push(format!(
            "{too_fast} of {} reps took less than {lo:.1} s; movement may be rushed.",
            full.len()
        ));
    }
    if too_slow as f64 > 0.3 * n {
        s.notes.push(format!(
            "{too_slow} of {} reps took longer than {hi:.0} s.",
            full.len()
        ));
    }
    if rushed_ecc as f64 > 0.5 * n {
        s.notes.push(format!(
            "Lowering (eccentric) phase is much quicker than lifting on {rushed_ecc} of {} reps; slow the return.",
            full.len()
        ));
    }    let parts = [
        (s.scores.consistency, 0.40),
        (s.scores.form, 0.30),
        (s.scores.tempo, 0.30),
    ];
    let wsum: f64 = parts.iter().filter(|p| p.0.is_some()).map(|p| p.1).sum();
    if wsum > 0.0 {
        s.scores.overall = Some(
            parts
                .iter()
                .filter_map(|p| p.0.map(|v| v * p.1))
                .sum::<f64>()
                / wsum,
        );
    }

    for st in sets {
        if let Some(d) = st.rom_drop_pct.filter(|d| *d > 15.0) {
            s.notes.push(format!(
                "Set {}: range of motion dropped {d:.0}% from first to last reps (fatigue?).",
                st.index
            ));
        }
        if let Some(d) = st.speed_loss_pct.filter(|d| *d > 20.0) {
            s.notes.push(format!(
                "Set {}: speed dropped {d:.0}%, a sign the set was near failure.",
                st.index
            ));
        }
    }
    if ex.rep_note.starts_with("Right wrist only") {
        s.notes
            .push("Counted from the right arm only: each rep is one left + one right.".into());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exercises;

    /// Curls: forearm swings 0..120 degrees `reps` times, 2.5 s each, with rest around them.
    fn curls(reps: usize) -> ImuSeries {
        let hz = 50.0;
        let rep_s = 2.5;
        let total = 2.0 + reps as f64 * rep_s + 2.0;
        let n = (total * hz) as usize;
        let angle = |t: f64| {
            let u = t - 2.0;
            if u <= 0.0 || u >= reps as f64 * rep_s {
                0.0
            } else {
                60.0 * (1.0 - (2.0 * std::f64::consts::PI * u / rep_s).cos())
            }
        };
        let mut s = ImuSeries {
            hz,
            t: vec![],
            acc: vec![],
            gyr: vec![],
            gaps: 0,
            valid: vec![true; n],
            spikes: 0,
            gyro_bias: None,
        };
        for i in 0..n {
            let t = i as f64 / hz;
            let a = angle(t).to_radians();
            let rate = (angle(t + 0.5 / hz) - angle(t - 0.5 / hz)) * hz;
            s.t.push(t);
            s.acc.push([a.sin(), 0.0, a.cos()]);
            s.gyr.push([0.0, -rate, 0.0]);
        }
        s
    }

    fn prefix(s: &ImuSeries, k: usize) -> ImuSeries {
        ImuSeries {
            hz: s.hz,
            t: s.t[..k].to_vec(),
            acc: s.acc[..k].to_vec(),
            gyr: s.gyr[..k].to_vec(),
            gaps: 0,
            valid: s.valid[..k].to_vec(),
            spikes: 0,
            gyro_bias: None,
        }
    }

    #[test]
    fn counts_each_movement_peak_once() {
        let ex = exercises::find("bicep_curls").unwrap();
        let s = curls(10);
        let a = analyze(&s, ex, None, None);
        assert_eq!(a.reps.len(), 10);
        assert!((a.period_s - 2.5).abs() < 0.3, "period {}", a.period_s);
        for r in &a.reps {
            assert!((r.rom - 120.0).abs() < 15.0, "rom {}", r.rom);
        }
    }

    #[test]
    fn live_count_never_goes_down() {
        let ex = exercises::find("bicep_curls").unwrap();
        let s = curls(10);
        let mut prev = Some(Analysis::recording_start());
        let mut last = 0;
        for k in (50..=s.t.len()).step_by(50) {
            let a = analyze(&prefix(&s, k), ex, None, prev.as_ref());
            assert!(a.reps.len() >= last, "count went from {last} to {}", a.reps.len());
            last = a.reps.len();
            prev = Some(a);
        }
        assert_eq!(last, 10);
    }
}