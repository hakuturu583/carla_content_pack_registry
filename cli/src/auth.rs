//! GitHub OAuth device flow (https://docs.github.com/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow).
//! The CLI never sees the OAuth client secret; the registry accepts the resulting
//! token as `Authorization: Bearer`.
use std::time::Duration;

use reqwest::blocking::Client as Http;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Login finished; contains the access token.
    Token(String),
    /// The user has not finished yet; poll again after the interval.
    Pending,
    /// GitHub asks to slow down; use the new interval (seconds).
    SlowDown(u64),
}

pub struct DeviceFlow {
    github: String,
    client_id: String,
    http: Http,
}

impl DeviceFlow {
    /// `github_url` is `https://github.com` except in tests.
    pub fn new(github_url: &str, client_id: &str) -> Result<Self> {
        let http = Http::builder()
            .user_agent(concat!("carla-pack/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { github: github_url.trim_end_matches('/').to_string(), client_id: client_id.to_string(), http })
    }

    pub fn start(&self, scope: &str) -> Result<DeviceCode> {
        let res = self
            .http
            .post(format!("{}/login/device/code", self.github))
            .header("accept", "application/json")
            .form(&[("client_id", self.client_id.as_str()), ("scope", scope)])
            .send()?;
        let body: serde_json::Value = res.json()?;
        if let Some(e) = body["error"].as_str() {
            let hint =
                if e == "device_flow_disabled" { " (enable Device Flow in the GitHub OAuth App settings)" } else { "" };
            return Err(Error::Usage(format!("GitHub refused device login: {e}{hint}")));
        }
        Ok(serde_json::from_value(body)?)
    }

    /// One poll of the token endpoint. Call it every `interval` seconds until it returns a token.
    pub fn poll(&self, device_code: &str) -> Result<Poll> {
        let res = self
            .http
            .post(format!("{}/login/oauth/access_token", self.github))
            .header("accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()?;
        let body: serde_json::Value = res.json()?;
        if let Some(token) = body["access_token"].as_str() {
            return Ok(Poll::Token(token.to_string()));
        }
        match body["error"].as_str() {
            Some("authorization_pending") => Ok(Poll::Pending),
            Some("slow_down") => Ok(Poll::SlowDown(body["interval"].as_u64().unwrap_or(10))),
            Some("expired_token") => Err(Error::Usage("the login code expired; run `carla-pack login` again".into())),
            Some("access_denied") => Err(Error::Usage("login was cancelled in the browser".into())),
            Some(other) => Err(Error::Usage(format!("GitHub login failed: {other}"))),
            None => Err(Error::Usage("unexpected response from GitHub".into())),
        }
    }
}
