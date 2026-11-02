//! The access check. Everything that proves a user may download a pack is a GitHub API
//! read of the pack repository with the user's own token: the manifest
//! (`carla-packs.toml`) and the storage credentials (a release asset). Without read access
//! GitHub answers 404 and nothing else is learnt.
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client as Http, RequestBuilder, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::manifest::{CREDENTIALS_ASSET, MANIFEST_FILE, Manifest};
use crate::sigv4::Credentials;
use crate::time;

const GITHUB_API: &str = "https://api.github.com";

/// Credentials are not used when they expire within this many seconds.
const EXPIRY_MARGIN: i64 = 120;

pub struct GitHub {
    api: String,
    token: Option<String>,
    http: Http,
}

/// `credentials.json`, the release asset the pack repository's credential workflow publishes.
#[derive(Deserialize)]
struct CredentialFile {
    version: u32,
    #[serde(flatten)]
    credentials: Credentials,
    #[serde(default)]
    expires_at: Option<String>,
}

/// Storage credentials read from a pack repository.
#[derive(Debug, Clone)]
pub struct Vended {
    pub credentials: Credentials,
    /// Unix seconds; `None` for long-lived keys.
    pub expires_at: Option<i64>,
}

impl GitHub {
    pub fn new(api: &str, token: Option<String>, http: Http) -> Self {
        Self { api: api.trim_end_matches('/').to_string(), token, http }
    }

    /// The API at `CARLA_PACK_GITHUB_API` (GitHub Enterprise Server), default api.github.com.
    pub fn default_api() -> String {
        std::env::var("CARLA_PACK_GITHUB_API").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| GITHUB_API.into())
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn get(&self, path: &str, accept: &str) -> RequestBuilder {
        let req = self
            .http
            .get(format!("{}{path}", self.api))
            .header("accept", accept)
            .header("x-github-api-version", "2022-11-28")
            .timeout(Duration::from_secs(60));
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    fn check(&self, res: Response, what: &str) -> Result<Response> {
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let message =
            res.json::<Value>().ok().and_then(|v| v["message"].as_str().map(str::to_string)).unwrap_or_default();
        Err(match status {
            StatusCode::NOT_FOUND => Error::NoAccess(what.to_string()),
            StatusCode::UNAUTHORIZED => Error::Unauthenticated(format!("GitHub rejected the token ({message})")),
            StatusCode::FORBIDDEN if self.token.is_none() && message.contains("rate limit") => {
                Error::Unauthenticated(format!("anonymous GitHub API rate limit reached ({message})"))
            }
            _ => Error::GitHub { status: status.as_u16(), message: format!("{what}: {message}") },
        })
    }

    /// `GET /user`: who the token belongs to.
    pub fn user(&self) -> Result<Value> {
        if self.token.is_none() {
            return Err(Error::Unauthenticated("no GitHub token".into()));
        }
        let res = self.get("/user", "application/vnd.github+json").send()?;
        Ok(self.check(res, "the GitHub user")?.json()?)
    }

    /// Reads and validates `carla-packs.toml` at the repository's default branch.
    pub fn manifest(&self, repo: &str) -> Result<Manifest> {
        let res =
            self.get(&format!("/repos/{repo}/contents/{MANIFEST_FILE}"), "application/vnd.github.raw+json").send()?;
        let text = self.check(res, &format!("{repo}/{MANIFEST_FILE}"))?.text()?;
        Manifest::parse(&text, &format!("{repo}/{MANIFEST_FILE}"))
    }

    /// Downloads `credentials.json` from the release `tag` of `repo`.
    pub fn credentials(&self, repo: &str, tag: &str) -> Result<Vended> {
        let what = format!("{repo} release {tag}");
        let res = self.get(&format!("/repos/{repo}/releases/tags/{tag}"), "application/vnd.github+json").send()?;
        let release: Value = match self.check(res, &what) {
            Err(Error::NoAccess(_)) => {
                return Err(Error::Credentials(format!(
                    "{repo} has no release {tag:?} holding storage credentials (or you cannot read the repository); \
                     its owner must run the credential workflow"
                )));
            }
            other => other?.json()?,
        };
        let asset = release["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"] == CREDENTIALS_ASSET))
            .and_then(|a| a["url"].as_str())
            .ok_or_else(|| {
                Error::Credentials(format!(
                    "release {tag:?} of {repo} has no {CREDENTIALS_ASSET} (it may be being replaced; try again)"
                ))
            })?;
        let asset_path = asset
            .strip_prefix(&self.api)
            .ok_or_else(|| Error::GitHub { status: 0, message: format!("unexpected asset URL {asset}") })?;
        // GitHub redirects to a signed download URL on another host; reqwest drops the
        // Authorization header on cross-host redirects, so the token never leaves GitHub.
        let res = self.get(asset_path, "application/octet-stream").send()?;
        let bytes = self.check(res, &what)?.bytes()?;
        let file: CredentialFile = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Credentials(format!("{CREDENTIALS_ASSET} of {repo} is invalid: {e}")))?;
        if file.version != 1 {
            return Err(Error::Credentials(format!(
                "{CREDENTIALS_ASSET} of {repo} has version {}; upgrade carla-pack",
                file.version
            )));
        }
        let expires_at = match file.expires_at.as_deref() {
            None => None,
            Some(text) => Some(time::parse_rfc3339(text).ok_or_else(|| {
                Error::Credentials(format!("{CREDENTIALS_ASSET} of {repo} has an invalid expires_at {text:?}"))
            })?),
        };
        if let Some(t) = expires_at.filter(|t| *t < time::now() + EXPIRY_MARGIN) {
            return Err(Error::Credentials(format!(
                "the storage credentials of {repo} expired at {}; its credential workflow has not run since \
                 (ask the owner, or run it from the Actions tab if you can)",
                time::rfc3339(t)
            )));
        }
        Ok(Vended { credentials: file.credentials, expires_at })
    }
}
