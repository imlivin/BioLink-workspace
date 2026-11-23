//! Small signal-processing toolkit: zero-phase Butterworth filters, integration,
//! peak detection with prominence, 3x3 PCA and helpers.

use std::f64::consts::PI;

pub type V3 = [f64; 3];

pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn normalize(a: V3) -> V3 {
    let n = norm(a);
    if n > 1e-12 {
        scale(a, 1.0 / n)
    } else {
        [0.0, 0.0, 1.0]
    }
}

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn butter(fc: f64, fs: f64, highpass: bool) -> Self {
        let fc = fc.clamp(1e-4, fs * 0.45);
        let w0 = 2.0 * PI * fc / fs;
        let (s, c) = w0.sin_cos();
        let alpha = s / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let (b0, b1) = if highpass {
            ((1.0 + c) / 2.0, -(1.0 + c))
        } else {
            ((1.0 - c) / 2.0, 1.0 - c)
        };
        Biquad {
            b: [b0 / a0, b1 / a0, b0 / a0],
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
        }
    }

    fn run(&self, x: &mut [f64]) {
        if x.is_empty() {
            return;
        }
        // start in steady state for a constant input equal to x[0]
        let dc_gain = (self.b[0] + self.b[1] + self.b[2]) / (1.0 + self.a[0] + self.a[1]);
        let (mut x1, mut x2) = (x[0], x[0]);
        let (mut y1, mut y2) = (x[0] * dc_gain, x[0] * dc_gain);
        for v in x.iter_mut() {
            let x0 = *v;
            let y0 =
                self.b[0] * x0 + self.b[1] * x1 + self.b[2] * x2 - self.a[0] * y1 - self.a[1] * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;
            *v = y0;
        }
    }
}

fn filtfilt(x: &[f64], f: Biquad, pad: usize) -> Vec<f64> {
    let n = x.len();
    if n < 3 {
        return x.to_vec();
    }
    let pad = pad.min(n - 1);
    // odd reflection at both ends reduces edge transients
    let mut y = Vec::with_capacity(n + 2 * pad);
    for i in (1..=pad).rev() {
        y.push(2.0 * x[0] - x[i]);
    }
    y.extend_from_slice(x);
    for i in 1..=pad {
        y.push(2.0 * x[n - 1] - x[n - 1 - i]);
    }
    f.run(&mut y);
    y.reverse();
    f.run(&mut y);
    y.reverse();
    y[pad..pad + n].to_vec()
}

/// Zero-phase 2nd-order Butterworth low-pass (4th order effective).
pub fn lowpass(x: &[f64], fc: f64, fs: f64) -> Vec<f64> {
    let pad = ((3.0 * fs / fc) as usize).max(6);
    filtfilt(x, Biquad::butter(fc, fs, false), pad)
}

/// Zero-phase 2nd-order Butterworth high-pass.
pub fn highpass(x: &[f64], fc: f64, fs: f64) -> Vec<f64> {
    let pad = ((3.0 * fs / fc) as usize).max(6);
    filtfilt(x, Biquad::butter(fc, fs, true), pad)
}

pub fn bandpass(x: &[f64], lo: f64, hi: f64, fs: f64) -> Vec<f64> {
    lowpass(&highpass(x, lo, fs), hi, fs)
}

/// Cumulative trapezoidal integral.
pub fn integrate(x: &[f64], dt: f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(x.len());
    let mut acc = 0.0;
    for i in 0..x.len() {
        if i > 0 {
            acc += 0.5 * (x[i] + x[i - 1]) * dt;
        }
        out.push(acc);
    }
    out
}

/// Central-difference derivative.
pub fn derivative(x: &[f64], dt: f64) -> Vec<f64> {
    let n = x.len();
    if n < 2 {
        return vec![0.0; n];
    }
    (0..n)
        .map(|i| {
            let a = x[i.saturating_sub(1)];
            let b = x[(i + 1).min(n - 1)];
            let span = ((i + 1).min(n - 1) - i.saturating_sub(1)) as f64;
            (b - a) / (span * dt)
        })
        .collect()
}

/// Indices of peaks with at least `min_prom` prominence, at least `min_dist` samples apart.
pub fn find_peaks(x: &[f64], min_prom: f64, min_dist: usize) -> Vec<usize> {
    let n = x.len();
    if n < 3 {
        return vec![];
    }
    let mut cands: Vec<(usize, f64)> = Vec::new();
    let mut i = 1;
    while i < n - 1 {
        if x[i] > x[i - 1] {
            // handle flat tops: walk to the end of the plateau
            let mut j = i;
            while j + 1 < n && x[j + 1] == x[i] {
                j += 1;
            }
            if j + 1 < n && x[j + 1] < x[i] {
                let p = (i + j) / 2;
                let prom = prominence(x, p);
                if prom >= min_prom {
                    cands.push((p, prom));
                }
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    // keep the highest peaks first, drop neighbours closer than min_dist
    cands.sort_by(|a, b| {
        x[b.0]
            .partial_cmp(&x[a.0])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<usize> = Vec::new();
    for (p, _) in cands {
        if kept.iter().all(|&k| k.abs_diff(p) >= min_dist) {
            kept.push(p);
        }
    }
    kept.sort_unstable();
    kept
}

/// How far a peak stands above the higher of the two lowest points on either side of it
/// (before the signal rises above the peak again).
pub fn prominence(x: &[f64], p: usize) -> f64 {
    let h = x[p];
    let mut left_min = h;
    for i in (0..p).rev() {
        if x[i] > h {
            break;
        }
        left_min = left_min.min(x[i]);
    }
    let mut right_min = h;
    for &v in &x[p + 1..] {
        if v > h {
            break;
        }
        right_min = right_min.min(v);
    }
    h - left_min.max(right_min)
}

pub fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f64>() / x.len() as f64
    }
}

pub fn std_dev(x: &[f64]) -> f64 {
    if x.len() < 2 {
        return 0.0;
    }
    let m = mean(x);
    (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (x.len() - 1) as f64).sqrt()
}

/// Coefficient of variation (std / |mean|).
pub fn cv(x: &[f64]) -> f64 {
    let m = mean(x).abs();
    if m < 1e-9 { 0.0 } else { std_dev(x) / m }
}

/// Coefficient of variation that ignores a few outliers (e.g. one rep next to a data gap):
/// 1.4826 x median absolute deviation / |median|. Equals std/mean for normal data.
pub fn robust_cv(x: &[f64]) -> f64 {
    if x.len() < 3 {
        return cv(x);
    }
    let m = median(x);
    let mad = median(&x.iter().map(|v| (v - m).abs()).collect::<Vec<_>>());
    if m.abs() < 1e-9 { 0.0 } else { 1.4826 * mad / m.abs() }
}

pub fn rms(x: &[f64]) -> f64 {
    if x.is_empty() {
        0.0
    } else {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }
}

pub fn percentile(x: &[f64], p: f64) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    let mut v = x.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((p / 100.0) * (v.len() - 1) as f64).round() as usize;
    v[idx.min(v.len() - 1)]
}

pub fn median(x: &[f64]) -> f64 {
    percentile(x, 50.0)
}

pub fn argmin(x: &[f64]) -> usize {
    let mut best = 0;
    for i in 1..x.len() {
        if x[i] < x[best] {
            best = i;
        }
    }
    best
}

/// Linear resampling of `x` to `n` points.
pub fn resample(x: &[f64], n: usize) -> Vec<f64> {
    if x.is_empty() {
        return vec![0.0; n];
    }
    if x.len() == 1 || n < 2 {
        return vec![x[0]; n];
    }
    (0..n)
        .map(|i| {
            let pos = i as f64 * (x.len() - 1) as f64 / (n - 1) as f64;
            let k = pos.floor() as usize;
            let f = pos - k as f64;
            if k + 1 < x.len() {
                x[k] * (1.0 - f) + x[k + 1] * f
            } else {
                x[x.len() - 1]
            }
        })
        .collect()
}

pub fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (ma, mb) = (mean(&a[..n]), mean(&b[..n]));
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (da, db) = (a[i] - ma, b[i] - mb);
        sab += da * db;
        saa += da * da;
        sbb += db * db;
    }
    if saa < 1e-12 || sbb < 1e-12 {
        0.0
    } else {
        sab / (saa * sbb).sqrt()
    }
}

/// Principal axes of a 3-D point cloud. Returns (eigenvalues desc, eigenvectors) and the mean.
pub fn pca3(points: &[V3]) -> ([f64; 3], [V3; 3], V3) {
    let n = points.len().max(1) as f64;
    let mut m = [0.0; 3];
    for p in points {
        for k in 0..3 {
            m[k] += p[k] / n;
        }
    }
    let mut c = [[0.0; 3]; 3];
    for p in points {
        let d = sub(*p, m);
        for i in 0..3 {
            for j in 0..3 {
                c[i][j] += d[i] * d[j] / n;
            }
        }
    }
    let (vals, vecs) = jacobi_eigen(c);
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| {
        vals[b]
            .partial_cmp(&vals[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let col = |k: usize| [vecs[0][k], vecs[1][k], vecs[2][k]];
    (
        [vals[idx[0]], vals[idx[1]], vals[idx[2]]],
        [col(idx[0]), col(idx[1]), col(idx[2])],
        m,
    )
}

fn jacobi_eigen(mut a: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        let (mut p, mut q, mut max) = (0, 1, 0.0);
        for i in 0..3 {
            for j in (i + 1)..3 {
                if a[i][j].abs() > max {
                    max = a[i][j].abs();
                    p = i;
                    q = j;
                }
            }
        }
        if max < 1e-15 {
            break;
        }
        let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for k in 0..3 {
            let akp = a[k][p];
            let akq = a[k][q];
            a[k][p] = c * akp - s * akq;
            a[k][q] = s * akp + c * akq;
        }
        for k in 0..3 {
            let apk = a[p][k];
            let aqk = a[q][k];
            a[p][k] = c * apk - s * aqk;
            a[q][k] = s * apk + c * aqk;
        }
        for k in 0..3 {
            let vkp = v[k][p];
            let vkq = v[k][q];
            v[k][p] = c * vkp - s * vkq;
            v[k][q] = s * vkp + c * vkq;
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}
