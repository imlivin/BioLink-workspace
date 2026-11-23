//! Minimal read-only Firebase client: anonymous Auth + Realtime Database REST (GET and event streams).

use crate::config::Config;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Token {
    id: String,
    refresh: String,
    expires: Instant,
}

pub struct Firebase {
    http: reqwest::blocking::Client,
    /// No overall timeout: event streams stay open for hours.
    stream_http: reqwest::blocking::Client,
    db_url: String,
    api_key: String,
    /// The anonymous account's refresh token is saved here and reused between launches.
    token_file: PathBuf,
    token: Mutex<Option<Token>>,
}

impl Firebase {
    pub fn new(cfg: &Config, token_file: PathBuf) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        let stream_http = reqwest::blocking::Client::builder()
            .timeout(None)
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Firebase {
            http,
            stream_http,
            db_url: cfg.database_url.clone(),
            api_key: cfg.api_key.clone(),
            token_file,
            token: Mutex::new(None),
        })
    }

    fn sign_in(&self) -> Result<Token> {
        if let Ok(saved) = std::fs::read_to_string(&self.token_file) {
            if let Ok(t) = self.refresh(saved.trim()) {
                return Ok(t);
            }
        }
        let url = format!(
            "https://identitytoolkit.googleapis.com/v1/accounts:signUp?key={}",
            self.api_key
        );
        let resp = self
            .http
            .post(url)
            .json(&serde_json::json!({ "returnSecureToken": true }))
            .send()?;
        let status = resp.status();
        let v: Value = resp.json().unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = v["error"]["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string();
            let hint = match msg.as_str() {
                "ADMIN_ONLY_OPERATION" | "OPERATION_NOT_ALLOWED" => {
                    " (enable Anonymous in Firebase console > Authentication > Sign-in method)"
                }
                m if m.starts_with("API_KEY") => " (check api_key in biolink.toml)",
                _ => "",
            };
            bail!("Firebase sign-in failed: {msg}{hint}");
        }
        let t = Token {
            id: v["idToken"]
                .as_str()
                .ok_or_else(|| anyhow!("no idToken in response"))?
                .to_string(),
            refresh: v["refreshToken"].as_str().unwrap_or_default().to_string(),
            expires: Instant::now() + Duration::from_secs(parse_secs(&v["expiresIn"])),
        };
        let _ = std::fs::write(&self.token_file, &t.refresh);
        Ok(t)
    }

    fn refresh(&self, refresh_token: &str) -> Result<Token> {
        let url = format!(
            "https://securetoken.googleapis.com/v1/token?key={}",
            self.api_key
        );
        let resp = self
            .http
            .post(url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()?;
        if !resp.status().is_success() {
            bail!("token refresh failed: {}", resp.status());
        }
        let v: Value = resp.json()?;
        let t = Token {
            id: v["id_token"]
                .as_str()
                .ok_or_else(|| anyhow!("no id_token"))?
                .to_string(),
            refresh: v["refresh_token"]
                .as_str()
                .unwrap_or(refresh_token)
                .to_string(),
            expires: Instant::now() + Duration::from_secs(parse_secs(&v["expires_in"])),
        };
        let _ = std::fs::write(&self.token_file, &t.refresh);
        Ok(t)
    }

    fn id_token(&self) -> Result<String> {
        let mut guard = self
            .token
            .lock()
            .map_err(|_| anyhow!("token lock poisoned"))?;
        let fresh = guard
            .as_ref()
            .is_some_and(|t| t.expires > Instant::now() + Duration::from_secs(300));
        if !fresh {
            let new = match guard.as_ref() {
                Some(old) => self.refresh(&old.refresh).or_else(|_| self.sign_in())?,
                None => self.sign_in()?,
            };
            *guard = Some(new);
        }
        Ok(guard.as_ref().map(|t| t.id.clone()).unwrap_or_default())
    }

    fn invalidate(&self) {
        if let Ok(mut g) = self.token.lock() {
            *g = None;
        }
    }

    /// Open a Realtime Database event stream (server-sent events) for `path`.
    /// The connection stays open; Firebase pushes every change as it happens.
    pub fn open_stream(&self, path: &str) -> Result<reqwest::blocking::Response> {
        let url = format!("{}/{}.json", self.db_url, path.trim_matches('/'));
        let token = self.id_token()?;
        let resp = self
            .stream_http
            .get(&url)
            .query(&[("auth", token.as_str())])
            .header("Accept", "text/event-stream")
            .send()
            .with_context(|| format!("opening stream /{path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            if status.as_u16() == 401 {
                self.invalidate();
            }
            bail!(
                "stream /{path} failed ({status}): {}",
                resp.text().unwrap_or_default().trim()
            );
        }
        Ok(resp)
    }
}

fn parse_secs(v: &Value) -> u64 {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64())
        .unwrap_or(3600)
}
