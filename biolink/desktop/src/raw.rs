//! Exercise-independent interpretation of the raw sensor data: physical units, magnitudes,
//! tilt, integrated rotation, vertical motion, statistics and the frequency content.
//! Nothing here depends on which exercise was selected.

use crate::analysis::gravity_track;
use crate::dsp::{self, V3};
use crate::model::{self, Sample, SessionInfo};

const G: f64 = 9.80665;

#[derive(Clone, Debug, Default)]
pub struct AxisStats {
    pub name: &'static str,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub std: f64,
    pub rms: f64,
}

impl AxisStats {
    fn of(name: &'static str, x: &[f64]) -> Self {
        AxisStats {
            name,
            mean: dsp::mean(x),
            min: x.iter().cloned().fold(f64::INFINITY, f64::min),
            max: x.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            std: dsp::std_dev(x),
            rms: dsp::rms(x),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RawView {
    pub hz: f64,
    pub samples: usize,
    pub gaps: usize,
    /// Single-sample glitches that were replaced.
    pub spikes: usize,
    pub t: Vec<f64>,
    /// Acceleration as measured (g), per axis, and its magnitude.
    pub acc: Vec<V3>,
    pub acc_mag: Vec<f64>,
    /// Rotation speed as measured (deg/s), per axis, and its magnitude.
    pub gyr: Vec<V3>,
    pub gyr_mag: Vec<f64>,
    /// Tilt of the board from the accelerometer alone (deg).
    pub pitch: Vec<f64>,
    pub roll: Vec<f64>,
    /// Integrated rotation per axis (deg).
    pub angle: Vec<V3>,
    /// Vertical motion of the wrist with gravity removed.
    pub up_acc: Vec<f64>,
    pub up_vel: Vec<f64>,
    pub up_pos_cm: Vec<f64>,
    /// Horizontal acceleration magnitude (m/s²).
    pub horiz_acc: Vec<f64>,
    /// (frequency Hz, relative power 0..1) of the most active accel and gyro axis.
    pub acc_spectrum: Vec<[f64; 2]>,
    pub gyr_spectrum: Vec<[f64; 2]>,
    /// Index (0 = X, 1 = Y, 2 = Z) of the axis with the most variation.
    pub acc_axis: usize,
    pub gyr_axis: usize,
    pub acc_stats: Vec<AxisStats>,
    pub gyr_stats: Vec<AxisStats>,
    pub gyro_offset: Option<V3>,
    pub moving_pct: f64,
    pub peak_acc_g: f64,
    pub peak_gyr_dps: f64,
    pub total_rotation_deg: f64,
    pub vertical_range_cm: f64,
    /// Strongest repeating rhythm in the movement (Hz), and how many cycles that implies.
    pub rhythm_hz: Option<f64>,
    pub rhythm_cycles: Option<f64>,
}

impl RawView {
    pub fn duration(&self) -> f64 {
        match (self.t.first(), self.t.last()) {
            (Some(a), Some(b)) => b - a,
            _ => 0.0,
        }
    }
}

/// Gyro offset correction and drift removal are always applied: without them integrated
/// rotation and height run away within seconds.
pub fn compute(info: &SessionInfo, samples: &[Sample]) -> RawView {
    let mut pts = model::to_units(info, samples);
    let spikes = model::despike(&mut pts);
    let hz = model::sample_rate(&pts);
    let offset = model::gyro_bias(&pts, hz);
    let series = model::resample_uniform(&pts, hz);
    let n = series.t.len();
    let mut v = RawView {
        hz,
        samples: samples.len(),
        gaps: series.gaps,
        spikes,
        gyro_offset: offset,
        ..Default::default()
    };
    if n < 2 {
        return v;
    }
    let dt = 1.0 / hz;
    let col = |d: &[V3], k: usize| d.iter().map(|x| x[k]).collect::<Vec<f64>>();

    v.acc_mag = series.acc.iter().map(|a| dsp::norm(*a)).collect();
    v.gyr_mag = series.gyr.iter().map(|g| dsp::norm(*g)).collect();
    v.pitch = series
        .acc
        .iter()
        .map(|a| {
            (-a[0])
                .atan2((a[1] * a[1] + a[2] * a[2]).sqrt())
                .to_degrees()
        })
        .collect();
    v.roll = series
        .acc
        .iter()
        .map(|a| a[1].atan2(a[2]).to_degrees())
        .collect();

    // integrated rotation per axis
    let off = offset.unwrap_or([0.0; 3]);
    let corrected: Vec<V3> = series.gyr.iter().map(|g| dsp::sub(*g, off)).collect();
    let ang: Vec<Vec<f64>> = (0..3)
        .map(|k| dsp::integrate(&col(&corrected, k), dt))
        .collect();
    v.angle = (0..n).map(|i| [ang[0][i], ang[1][i], ang[2][i]]).collect();
    v.total_rotation_deg = corrected.iter().map(|g| dsp::norm(*g) * dt).sum();

    // vertical / horizontal motion: needs the direction of gravity, tracked with gyro + accel
    let mut for_grav = series.clone();
    for_grav.gyr = corrected.clone();
    let grav = gravity_track(&for_grav, hz);
    v.up_acc = series
        .acc
        .iter()
        .zip(&grav)
        .map(|(a, g)| (dsp::dot(*a, *g) - 1.0) * G)
        .collect();
    v.horiz_acc = series
        .acc
        .iter()
        .zip(&grav)
        .map(|(a, g)| {
            let lin = dsp::sub(*a, *g);
            let h = dsp::sub(lin, dsp::scale(*g, dsp::dot(lin, *g)));
            dsp::norm(h) * G
        })
        .collect();
    let hp = |x: Vec<f64>| dsp::highpass(&x, 0.1, hz);
    let a_up = hp(v.up_acc.clone());
    v.up_vel = hp(dsp::integrate(&a_up, dt));
    v.up_pos_cm = hp(dsp::integrate(&v.up_vel, dt))
        .iter()
        .map(|p| p * 100.0)
        .collect();
    v.vertical_range_cm = dsp::percentile(&v.up_pos_cm, 98.0) - dsp::percentile(&v.up_pos_cm, 2.0);

    // statistics
    v.acc_stats = vec![
        AxisStats::of("X", &col(&series.acc, 0)),
        AxisStats::of("Y", &col(&series.acc, 1)),
        AxisStats::of("Z", &col(&series.acc, 2)),
        AxisStats::of("Magnitude", &v.acc_mag),
    ];
    v.gyr_stats = vec![
        AxisStats::of("X", &col(&series.gyr, 0)),
        AxisStats::of("Y", &col(&series.gyr, 1)),
        AxisStats::of("Z", &col(&series.gyr, 2)),
        AxisStats::of("Magnitude", &v.gyr_mag),
    ];
    let moving = (0..n)
        .filter(|&i| v.gyr_mag[i] > 15.0 || (v.acc_mag[i] - 1.0).abs() > 0.1)
        .count();
    v.moving_pct = moving as f64 / n as f64 * 100.0;
    v.peak_acc_g = v.acc_mag.iter().cloned().fold(0.0, f64::max);
    v.peak_gyr_dps = v.gyr_mag.iter().cloned().fold(0.0, f64::max);

    // frequency content of the most active axis (signed, so one back-and-forth = one cycle)
    let busiest = |stats: &[AxisStats]| {
        (0..3)
            .max_by(|&a, &b| {
                stats[a]
                    .std
                    .partial_cmp(&stats[b].std)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(0)
    };
    v.acc_axis = busiest(&v.acc_stats);
    v.gyr_axis = busiest(&v.gyr_stats);
    v.acc_spectrum = spectrum(&col(&series.acc, v.acc_axis), hz);
    v.gyr_spectrum = spectrum(&col(&series.gyr, v.gyr_axis), hz);
    // rhythm: strongest peak between 0.1 and 3 Hz (rotation if the arm rotates, else acceleration)
    let spec = if v.gyr_stats[v.gyr_axis].std > 5.0 {
        &v.gyr_spectrum
    } else {
        &v.acc_spectrum
    };
    let best = spec
        .iter()
        .filter(|p| p[0] >= 0.1 && p[0] <= 3.0)
        .max_by(|a, b| a[1].partial_cmp(&b[1]).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(b) = best.filter(|b| b[1] > 0.0) {
        v.rhythm_hz = Some(b[0]);
        v.rhythm_cycles = Some(b[0] * v.duration() * v.moving_pct / 100.0);
    }

    v.t = series.t;
    v.acc = series.acc;
    v.gyr = series.gyr;
    v
}

/// Power spectrum (Hann window, mean removed) from 0.05 Hz to min(10 Hz, Nyquist),
/// normalized so the largest value is 1.
fn spectrum(x: &[f64], hz: f64) -> Vec<[f64; 2]> {
    let n = x.len();
    if n < 16 {
        return vec![];
    }
    let m = dsp::mean(x);
    let w: Vec<f64> = (0..n)
        .map(|i| {
            (x[i] - m)
                * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos())
        })
        .collect();
    let f_max = (hz / 2.0).min(10.0);
    let df = (hz / n as f64).max(0.02);
    let mut out = Vec::new();
    let mut f = 0.05;
    while f <= f_max {
        let step = 2.0 * std::f64::consts::PI * f / hz;
        let (mut re, mut im) = (0.0, 0.0);
        let (c, s) = (step.cos(), step.sin());
        let (mut cr, mut ci) = (1.0, 0.0);
        for v in &w {
            re += v * cr;
            im -= v * ci;
            let nr = cr * c - ci * s;
            ci = cr * s + ci * c;
            cr = nr;
        }
        out.push([f, re * re + im * im]);
        f += df;
    }
    let max = out.iter().map(|p| p[1]).fold(0.0, f64::max);
    if max > 0.0 {
        for p in &mut out {
            p[1] /= max;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_sensor_reads_one_g_and_finds_rhythm() {
        let info = SessionInfo::new(0, "x".into(), 16384.0, 131.0);
        // 20 s: still, then the board rotates back and forth about X at 0.5 Hz
        let samples: Vec<Sample> = (0..1000)
            .map(|i| {
                let t = i as f64 * 0.02;
                let rate = if t > 4.0 {
                    60.0 * (2.0 * std::f64::consts::PI * 0.5 * t).cos()
                } else {
                    0.0
                };
                Sample {
                    ts_ms: (t * 1000.0) as i64,
                    raw: [0, 0, 16384, (rate * 131.0) as i16 + 20, 0, 0],
                }
            })
            .collect();
        let v = compute(&info, &samples);
        assert!((v.acc_stats[3].mean - 1.0).abs() < 1e-3);
        assert!(v.gyro_offset.is_some(), "offset found in the still part");
        let r = v.rhythm_hz.unwrap();
        assert!((r - 0.5).abs() < 0.05, "rhythm {r}");
    }
}
