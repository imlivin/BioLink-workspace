//! Data model: what Firebase delivers, what PostgreSQL stores, and conversion of raw samples
//! into a uniformly sampled series for analysis.

use crate::dsp::{self, V3};
use serde_json::Value;

/// What the mobile app writes to /current when start or stop is pressed:
/// `{"exercise": "bicep_curls", "timestamp": <epoch ms>}` on start, `{"exercise": "none", ...}` on stop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppState {
    /// Catalog id, or None for "none" / missing.
    pub exercise: Option<String>,
    pub timestamp_ms: Option<i64>,
    /// The exercise text exactly as sent.
    pub raw_exercise: String,
}

impl AppState {
    /// Accepts the object form, or a bare string such as "none".
    pub fn parse(v: &Value) -> AppState {
        match v {
            Value::String(s) => AppState {
                exercise: normalize_exercise(s),
                timestamp_ms: None,
                raw_exercise: s.trim().to_string(),
            },
            Value::Object(m) => {
                let raw = m
                    .get("exercise")
                    .and_then(|e| e.as_str())
                    .unwrap_or("none")
                    .trim()
                    .to_string();
                AppState {
                    exercise: normalize_exercise(&raw),
                    timestamp_ms: m.get("timestamp").and_then(as_f64).map(|t| t as i64),
                    raw_exercise: raw,
                }
            }
            _ => AppState {
                raw_exercise: "none".into(),
                ..Default::default()
            },
        }
    }
}

/// One raw sample as stored in PostgreSQL.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub ts_ms: i64,
    pub raw: [i16; 6],
}

/// A row of the `sessions` table. A session runs from the phone's start press to its stop press.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    /// = start-press timestamp (epoch ms)
    pub id: i64,
    pub exercise: String,
    /// Start press (phone clock, epoch ms).
    pub started_ms: i64,
    /// Stop press; None while the exercise is still running.
    pub stopped_ms: Option<i64>,
    /// Time of the newest stored sample.
    pub last_sample_ms: Option<i64>,
    pub samples: i64,
    pub accel_lsb_per_g: f64,
    pub gyro_lsb_per_dps: f64,
}

impl SessionInfo {
    pub fn new(id: i64, exercise: String, accel_lsb_per_g: f64, gyro_lsb_per_dps: f64) -> Self {
        SessionInfo {
            id,
            exercise,
            started_ms: id,
            stopped_ms: None,
            last_sample_ms: None,
            samples: 0,
            accel_lsb_per_g,
            gyro_lsb_per_dps,
        }
    }

    pub fn in_progress(&self) -> bool {
        self.stopped_ms.is_none()
    }

    /// Start to stop press (or to the newest sample while running), in seconds.
    pub fn duration_s(&self) -> f64 {
        let end = self
            .stopped_ms
            .or(self.last_sample_ms)
            .unwrap_or(self.started_ms);
        (end - self.started_ms).max(0) as f64 / 1000.0
    }
}

/// Map what the phone sends ("bicep_curls", "Bicep Curls", "bent-over row") to a catalog-style id.
/// Returns None for "none" or empty, meaning "no exercise".
pub fn normalize_exercise(s: &str) -> Option<String> {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() || out == "none" || out == "null" {
        None
    } else {
        Some(out)
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn as_i16(v: &Value) -> Option<i16> {
    as_f64(v)
        .filter(|x| *x >= i16::MIN as f64 && *x <= i16::MAX as f64)
        .map(|x| x.round() as i16)
}

/// One sample from /test_imu. Accepts `{"ax":..,"ay":..,"az":..,"gx":..,"gy":..,"gz":..}`,
/// `[ax,ay,az,gx,gy,gz]` or `"ax,ay,az,gx,gy,gz"`.
fn parse_sample(v: &Value) -> Option<[i16; 6]> {
    const KEYS: [&str; 6] = ["ax", "ay", "az", "gx", "gy", "gz"];
    let vals: Vec<Option<i16>> = match v {
        Value::Object(m) => KEYS.iter().map(|k| m.get(*k).and_then(as_i16)).collect(),
        Value::Array(a) if a.len() == 6 => a.iter().map(as_i16).collect(),
        Value::String(s) => s.split(',').map(|x| x.trim().parse::<i16>().ok()).collect(),
        _ => return None,
    };
    if vals.len() != 6 {
        return None;
    }
    let mut out = [0i16; 6];
    for (o, v) in out.iter_mut().zip(vals) {
        *o = v?;
    }
    Some(out)
}

/// The /test_imu batch: indexes 0..N-1 in time order, 20 ms apart. Firebase returns it as an
/// array, or as an object with numeric keys; entries that can't be read are skipped.
pub fn parse_imu_batch(v: &Value) -> Vec<[i16; 6]> {
    match v {
        Value::Array(a) => a.iter().filter_map(parse_sample).collect(),
        Value::Object(m) => {
            let mut idx: Vec<(usize, &Value)> = m
                .iter()
                .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v)))
                .collect();
            idx.sort_by_key(|(i, _)| *i);
            idx.into_iter()
                .filter_map(|(_, v)| parse_sample(v))
                .collect()
        }
        _ => vec![],
    }
}

/// Share of raw values at the sensor's limit (the reading was clipped).
pub fn clip_fraction(samples: &[Sample]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let clipped = samples
        .iter()
        .filter(|s| s.raw.iter().any(|v| *v == i16::MAX || *v == i16::MIN))
        .count();
    clipped as f64 / samples.len() as f64
}

/// Uniformly sampled IMU data in physical units. `t` is seconds since the session start.
#[derive(Clone, Debug, Default)]
pub struct ImuSeries {
    pub hz: f64,
    pub t: Vec<f64>,
    /// g
    pub acc: Vec<V3>,
    /// deg/s, bias removed
    pub gyr: Vec<V3>,
    /// Gaps longer than 0.25 s in the received data.
    pub gaps: usize,
    /// false for samples inside a gap (filled with "no movement", not real data).
    pub valid: Vec<bool>,
    /// Single-sample glitches that were replaced.
    pub spikes: usize,
    /// Gyro bias found in the stillest second (deg/s), if the arm was still at some point.
    pub gyro_bias: Option<V3>,
}

impl ImuSeries {
    pub fn duration(&self) -> f64 {
        match (self.t.first(), self.t.last()) {
            (Some(a), Some(b)) => b - a,
            _ => 0.0,
        }
    }
}

/// Samples in physical units (t = s since the start press, accel in g, gyro in deg/s),
/// sorted, duplicates removed. No filtering.
pub fn to_units(info: &SessionInfo, samples: &[Sample]) -> Vec<(f64, V3, V3)> {
    let acc_scale = 1.0 / info.accel_lsb_per_g;
    let gyr_scale = 1.0 / info.gyro_lsb_per_dps;
    let mut pts: Vec<(f64, V3, V3)> = samples
        .iter()
        .map(|s| {
            let r = s.raw;
            (
                (s.ts_ms - info.started_ms) as f64 / 1000.0,
                [
                    r[0] as f64 * acc_scale,
                    r[1] as f64 * acc_scale,
                    r[2] as f64 * acc_scale,
                ],
                [
                    r[3] as f64 * gyr_scale,
                    r[4] as f64 * gyr_scale,
                    r[5] as f64 * gyr_scale,
                ],
            )
        })
        .collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    pts
}

/// Sample rate from the median spacing (nominally 50 Hz).
pub fn sample_rate(pts: &[(f64, V3, V3)]) -> f64 {
    let dts: Vec<f64> = pts
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|d| *d > 0.0 && *d < 0.25)
        .collect();
    if dts.is_empty() {
        50.0
    } else {
        (1.0 / dsp::median(&dts)).clamp(10.0, 200.0)
    }
}

/// Gyro offset: mean gyro over the stillest ~1 s window, if the arm was still enough.
pub fn gyro_bias(pts: &[(f64, V3, V3)], hz: f64) -> Option<V3> {
    let win = (hz as usize).max(10);
    let mut best: Option<(f64, V3)> = None;
    let mut i = 0;
    while i + win <= pts.len() {
        let w = &pts[i..i + win];
        let mut m = [0.0; 3];
        for p in w {
            m = dsp::add(m, dsp::scale(p.2, 1.0 / win as f64));
        }
        let var: f64 = w
            .iter()
            .map(|p| dsp::dot(dsp::sub(p.2, m), dsp::sub(p.2, m)))
            .sum::<f64>()
            / win as f64;
        if best.is_none_or(|b| var < b.0) {
            best = Some((var, m));
        }
        i += win / 2;
    }
    best.filter(|(var, m)| var.sqrt() < 2.0 && dsp::norm(*m) < 10.0)
        .map(|b| b.1)
}

/// Replace single-sample glitches (one reading far from both neighbours, which agree with each
/// other). Real movement changes over several samples, so it isn't affected.
/// Returns the number of samples replaced.
pub fn despike(pts: &mut [(f64, V3, V3)]) -> usize {
    const ACC_G: f64 = 0.35;
    const GYR_DPS: f64 = 60.0;
    let mut n = 0;
    for i in 1..pts.len().saturating_sub(1) {
        let (p, c, nx) = (pts[i - 1], pts[i], pts[i + 1]);
        let mut fixed = c;
        let mut hit = false;
        for k in 0..3 {
            let mid = 0.5 * (p.1[k] + nx.1[k]);
            if (c.1[k] - mid).abs() > ACC_G && (p.1[k] - nx.1[k]).abs() < ACC_G / 2.0 {
                fixed.1[k] = mid;
                hit = true;
            }
            let mid = 0.5 * (p.2[k] + nx.2[k]);
            if (c.2[k] - mid).abs() > GYR_DPS && (p.2[k] - nx.2[k]).abs() < GYR_DPS / 2.0 {
                fixed.2[k] = mid;
                hit = true;
            }
        }
        if hit {
            pts[i] = fixed;
            n += 1;
        }
    }
    n
}

/// Series used by the exercise analysis: glitches removed, uniform time grid, gyro offset removed.
pub fn build_series(info: &SessionInfo, samples: &[Sample]) -> ImuSeries {
    let mut pts = to_units(info, samples);
    let spikes = despike(&mut pts);
    let hz = sample_rate(&pts);
    let bias = gyro_bias(&pts, hz);
    if let Some(b) = bias {
        for p in pts.iter_mut() {
            p.2 = dsp::sub(p.2, b);
        }
    }
    let mut out = resample_uniform(&pts, hz);
    out.gyro_bias = bias;
    out.spikes = spikes;
    out
}

/// Gaps longer than this are not interpolated: they are filled with "no movement".
const MAX_INTERP_GAP_S: f64 = 0.25;

/// Linear interpolation onto a uniform time grid (no filtering). Inside gaps longer than
/// 0.25 s the wrist is treated as still (gravity only, no rotation) and marked invalid, so a
/// gap doesn't turn into a fake movement.
pub fn resample_uniform(pts: &[(f64, V3, V3)], hz: f64) -> ImuSeries {
    let mut out = ImuSeries {
        hz,
        ..Default::default()
    };
    if pts.len() < 2 {
        return out;
    }
    let dt = 1.0 / hz;
    let (t_first, t_last) = (pts[0].0, pts[pts.len() - 1].0);
    let n = ((t_last - t_first) / dt).floor() as usize + 1;
    let mut j = 0;
    for i in 0..n {
        let t = t_first + i as f64 * dt;
        while j + 2 < pts.len() && pts[j + 1].0 < t {
            j += 1;
        }
        let (p0, p1) = (&pts[j], &pts[(j + 1).min(pts.len() - 1)]);
        let span = p1.0 - p0.0;
        let f = if span > 1e-9 {
            ((t - p0.0) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let lerp = |a: V3, b: V3| {
            [
                a[0] + (b[0] - a[0]) * f,
                a[1] + (b[1] - a[1]) * f,
                a[2] + (b[2] - a[2]) * f,
            ]
        };
        out.t.push(t);
        if span > MAX_INTERP_GAP_S {
            // gravity direction blended between both sides, magnitude 1 g, no rotation
            let g = dsp::normalize(lerp(dsp::normalize(p0.1), dsp::normalize(p1.1)));
            out.acc.push(g);
            out.gyr.push([0.0; 3]);
            out.valid.push(false);
        } else {
            out.acc.push(lerp(p0.1, p1.1));
            out.gyr.push(lerp(p0.2, p1.2));
            out.valid.push(true);
        }
    }
    out.gaps = pts.windows(2).filter(|w| w[1].0 - w[0].0 > MAX_INTERP_GAP_S).count();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_test_imu_shapes() {
        let arr = json!([{"ax": 1, "ay": 2, "az": 3, "gx": 4, "gy": 5, "gz": 6}, null, [7, 8, 9, 10, 11, 12]]);
        assert_eq!(
            parse_imu_batch(&arr),
            vec![[1, 2, 3, 4, 5, 6], [7, 8, 9, 10, 11, 12]]
        );
        let obj = json!({"1": "7,8,9,10,11,12", "0": {"ax": 1, "ay": 2, "az": 3, "gx": 4, "gy": 5, "gz": 6}});
        assert_eq!(
            parse_imu_batch(&obj),
            vec![[1, 2, 3, 4, 5, 6], [7, 8, 9, 10, 11, 12]]
        );
    }

    #[test]
    fn despike_removes_single_glitches_but_keeps_movement() {
        let still = |t: f64| (t, [0.0, 0.0, 1.0], [0.0; 3]);
        let mut pts: Vec<(f64, V3, V3)> = (0..10).map(|i| still(i as f64 * 0.02)).collect();
        pts[4].1[2] = 0.0; // one reading drops to 0 g (seen in real data)
        pts[6].2[0] = 120.0; // one gyro spike
        assert_eq!(despike(&mut pts), 2);
        assert!((pts[4].1[2] - 1.0).abs() < 1e-9 && pts[6].2[0].abs() < 1e-9);
        // a real movement ramps over several samples and is left alone
        let mut ramp: Vec<(f64, V3, V3)> = (0..10).map(|i| (i as f64 * 0.02, [0.0, 0.0, 1.0 + 0.2 * i as f64], [0.0; 3])).collect();
        assert_eq!(despike(&mut ramp), 0);
    }

    #[test]
    fn long_gaps_are_filled_as_still_and_marked() {
        let mut pts: Vec<(f64, V3, V3)> = (0..10).map(|i| (i as f64 * 0.02, [0.0, 0.0, 1.4], [5.0; 3])).collect();
        pts.extend((0..10).map(|i| (1.2 + i as f64 * 0.02, [0.0, 0.0, 0.6], [5.0; 3])));
        let s = resample_uniform(&pts, 50.0);
        assert_eq!(s.gaps, 1);
        let mid = s.t.iter().position(|t| *t > 0.6).unwrap();
        assert!(!s.valid[mid]);
        assert!((dsp::norm(s.acc[mid]) - 1.0).abs() < 1e-9, "no fake acceleration inside the gap");
        assert_eq!(s.gyr[mid], [0.0; 3]);
        assert!(s.valid[0] && s.valid[s.t.len() - 1]);
    }

    #[test]
    fn parses_app_state() {
        let a = AppState::parse(&json!({"exercise": "Bicep Curls", "timestamp": 1762790000000i64}));
        assert_eq!(a.exercise.as_deref(), Some("bicep_curls"));
        assert_eq!(a.timestamp_ms, Some(1762790000000));
        assert_eq!(
            AppState::parse(&json!({"exercise": "none", "timestamp": 1})).exercise,
            None
        );
        assert_eq!(AppState::parse(&json!("none")).exercise, None);
    }
}
