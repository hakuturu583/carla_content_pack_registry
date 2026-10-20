//! GitHub OAuth device flow (https://docs.github.com/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow).
//! The CLI never sees the OAuth client secret; the registry accepts the resulting
//! token as `Authorization: Bearer`.
use std::time::{Duration, Instant};

use reqwest::blocking::{Client as Http, RequestBuilder};
use serde::Deserialize;

use crate::client::http_client;
use crate::error::{Error, Result};
use crate::store::normalize;

const GITHUB_URL: &str = "https://github.com";

#[derive(Debug, Clone, Deserialize)]
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
    /// GitHub at `CARLA_PACK_GITHUB_URL` (tests / GitHub Enterprise), default github.com.
    pub fn from_env(client_id: &str) -> Result<Self> {
        let url = std::env::var("CARLA_PACK_GITHUB_URL").ok().filter(|s| !s.is_empty());
        Self::new(url.as_deref().unwrap_or(GITHUB_URL), client_id)
    }

    pub fn new(github_url: &str, client_id: &str) -> Result<Self> {
        Ok(Self { github: normalize(github_url), client_id: client_id.to_string(), http: http_client()? })
    }

    fn post(&self, path: &str) -> RequestBuilder {
        self.http
            .post(format!("{}{path}", self.github))
            .header("accept", "application/json")
            .timeout(Duration::from_secs(30))
    }

    /// Polls until the user finishes in the browser, honouring `interval` and `slow_down`.
    /// `keep_going` is called about once a second; returning false aborts with `Interrupted`.
    pub fn wait(&self, code: &DeviceCode, mut keep_going: impl FnMut() -> bool) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = code.interval;
        loop {
            for _ in 0..interval.max(1) {
                if !keep_going() {
                    return Err(Error::Interrupted);
                }
                if interval > 0 {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Usage("the login code expired; run `carla-pack login` again".into()));
            }
            match self.poll(&code.device_code)? {
                Poll::Token(t) => return Ok(t),
                Poll::Pending => {}
                Poll::SlowDown(i) => interval = i,
            }
        }
    }

    pub fn start(&self, scope: &str) -> Result<DeviceCode> {
        let res =
            self.post("/login/device/code").form(&[("client_id", self.client_id.as_str()), ("scope", scope)]).send()?;
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
            .post("/login/oauth/access_token")
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
