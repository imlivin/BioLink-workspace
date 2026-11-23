//! Configuration file `biolink.toml` (see config.example.toml).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;

fn d_imu_path() -> String {
    "test_imu".into()
}
fn d_app_path() -> String {
    "current".into()
}
fn d_accel() -> f64 {
    16384.0
}
fn d_gyro() -> f64 {
    131.0
}
fn d_interval() -> f64 {
    20.0
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub database_url: String,
    pub api_key: String,
    /// e.g. postgresql://postgres:password@localhost:5432/biolink
    pub postgres_url: String,
    /// Node the ESP32 overwrites once per second with the latest batch of samples.
    #[serde(default = "d_imu_path")]
    pub imu_path: String,
    /// Node the mobile app writes {"exercise": "...", "timestamp": <epoch ms>} to on start/stop.
    #[serde(default = "d_app_path")]
    pub app_state_path: String,
    /// Raw counts per g (MPU-6050 default +-2 g = 16384).
    #[serde(default = "d_accel")]
    pub accel_lsb_per_g: f64,
    /// Raw counts per deg/s (MPU-6050 default +-250 deg/s = 131).
    #[serde(default = "d_gyro")]
    pub gyro_lsb_per_dps: f64,
    /// Time between samples inside a batch.
    #[serde(default = "d_interval")]
    pub sample_interval_ms: f64,
}

fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("BIOLINK_CONFIG") {
        v.push(PathBuf::from(p));
    }
    v.push(PathBuf::from("biolink.toml"));
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        v.push(dir.join("biolink.toml"));
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        v.push(PathBuf::from(appdata).join("BioLink").join("biolink.toml"));
    }
    v
}

pub fn load() -> Result<(Config, PathBuf)> {
    for path in candidates() {
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let mut cfg: Config =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            cfg.database_url = cfg.database_url.trim_end_matches('/').to_string();
            cfg.imu_path = cfg.imu_path.trim_matches('/').to_string();
            cfg.app_state_path = cfg.app_state_path.trim_matches('/').to_string();
            if cfg.api_key.trim().is_empty() || cfg.api_key.contains("...") {
                bail!("api_key in {} is not filled in", path.display());
            }
            if cfg.postgres_url.contains("YOUR_PASSWORD") {
                bail!(
                    "postgres_url in {} still contains YOUR_PASSWORD",
                    path.display()
                );
            }
            return Ok((cfg, path));
        }
    }
    let tried: Vec<String> = candidates()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    bail!(
        "biolink.toml not found. Copy config.example.toml to biolink.toml. Looked in:\n  {}",
        tried.join("\n  ")
    )
}

/// Local folder for the anonymous sign-in token.
pub fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let dir = base.join("BioLink");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
