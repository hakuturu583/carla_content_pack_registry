//! Local client state and the GitHub token lookup.
//!
//! `$CARLA_PACK_CONFIG_DIR/config.json` (default: the OS config directory, e.g.
//! `~/.config/carla-pack/`), readable only by the user on Unix: the saved login and the
//! pack repositories searched by map name.
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::manifest::validate_repo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Login {
    pub token: String,
    pub login: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<Login>,
    #[serde(default)]
    pub repos: Vec<String>,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("CARLA_PACK_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    dirs::config_dir()
        .map(|d| d.join("carla-pack"))
        .ok_or_else(|| Error::Usage("cannot determine a config directory; set CARLA_PACK_CONFIG_DIR".into()))
}

/// Where downloaded packs are kept, by SHA-256: `CARLA_PACK_CACHE_DIR`, else e.g. `~/.cache/carla-pack`.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("CARLA_PACK_CACHE_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    dirs::cache_dir()
        .map(|d| d.join("carla-pack"))
        .ok_or_else(|| Error::Usage("cannot determine a cache directory; set CARLA_PACK_CACHE_DIR".into()))
}

fn path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}

impl Store {
    pub fn load() -> Result<Self> {
        match fs::read(path()?) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self) -> Result<()> {
        let p = path()?;
        fs::create_dir_all(p.parent().expect("config path has a parent"))?;
        let tmp = p.with_extension("json.tmp");
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self)?)?;
        f.sync_all()?;
        fs::rename(tmp, p)?;
        Ok(())
    }

    /// Adds a repository to search; returns false when it was already there.
    pub fn add_repo(&mut self, repo: &str) -> Result<bool> {
        validate_repo(repo)?;
        if self.repos.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
            return Ok(false);
        }
        self.repos.push(repo.to_string());
        Ok(true)
    }

    pub fn remove_repo(&mut self, repo: &str) -> bool {
        let before = self.repos.len();
        self.repos.retain(|r| !r.eq_ignore_ascii_case(repo));
        self.repos.len() != before
    }

    /// Repositories searched by map name: `CARLA_PACK_REPOS` (comma-separated), else the saved list.
    pub fn search_repos(&self) -> Vec<String> {
        match std::env::var("CARLA_PACK_REPOS").ok().filter(|s| !s.trim().is_empty()) {
            Some(list) => list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
            None => self.repos.clone(),
        }
    }
}

/// A GitHub token and where it came from.
#[derive(Debug, Clone)]
pub struct Token {
    pub value: String,
    pub source: &'static str,
}

/// The first of: `CARLA_PACK_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, the saved login,
/// `gh auth token` (GitHub CLI). `None`: anonymous (public repositories only).
pub fn find_token(store: &Store) -> Option<Token> {
    for var in ["CARLA_PACK_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(v) = std::env::var(var).ok().filter(|v| !v.trim().is_empty()) {
            let source: &'static str = match var {
                "CARLA_PACK_TOKEN" => "CARLA_PACK_TOKEN",
                "GH_TOKEN" => "GH_TOKEN",
                _ => "GITHUB_TOKEN",
            };
            return Some(Token { value: v.trim().to_string(), source });
        }
    }
    if let Some(l) = &store.login {
        return Some(Token { value: l.token.clone(), source: "carla-pack login" });
    }
    if std::env::var("CARLA_PACK_NO_GH").is_ok_and(|v| !v.is_empty()) {
        return None;
    }
    let out = Command::new("gh").args(["auth", "token"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let value = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !value.is_empty()).then_some(Token { value, source: "gh auth token" })
}
