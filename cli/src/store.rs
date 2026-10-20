//! Local CLI state: the default registry and one token per registry URL.
//!
//! Stored as JSON in `$CARLA_PACK_CONFIG_DIR/credentials.json` (default: the OS config
//! directory, e.g. `~/.config/carla-pack/`), readable only by the user on Unix.
//! Environment: `CARLA_PACK_REGISTRY`, `CARLA_PACK_TOKEN` (override the saved values),
//! `CARLA_PACK_CONFIG_DIR`; `CARLA_PACK_GITHUB_URL` is read by auth.rs.
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Used when no registry has been configured yet (local development).
pub const FALLBACK_REGISTRY: &str = "http://localhost:8787";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Login {
    pub token: String,
    pub login: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    #[serde(default)]
    pub registries: BTreeMap<String, Login>,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("CARLA_PACK_CONFIG_DIR") {
        return Ok(PathBuf::from(d));
    }
    dirs::config_dir()
        .map(|d| d.join("carla-pack"))
        .ok_or_else(|| Error::Usage("cannot determine a config directory; set CARLA_PACK_CONFIG_DIR".into()))
}

fn path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

pub fn normalize(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
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
        fs::create_dir_all(p.parent().expect("credentials path has a parent"))?;
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

    /// Registry to use: explicit argument, then `CARLA_PACK_REGISTRY`, then the saved default.
    pub fn registry(&self, explicit: Option<&str>) -> String {
        explicit
            .map(str::to_string)
            .or_else(|| std::env::var("CARLA_PACK_REGISTRY").ok().filter(|s| !s.is_empty()))
            .or_else(|| self.default_registry.clone())
            .map(|u| normalize(&u))
            .unwrap_or_else(|| FALLBACK_REGISTRY.to_string())
    }

    /// Token for a registry: `CARLA_PACK_TOKEN`, then the saved login.
    pub fn token(&self, registry: &str) -> Option<String> {
        std::env::var("CARLA_PACK_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.registries.get(&normalize(registry)).map(|l| l.token.clone()))
    }

    pub fn set_login(&mut self, registry: &str, login: Login) {
        let r = normalize(registry);
        self.registries.insert(r.clone(), login);
        self.default_registry = Some(r);
    }

    /// Returns true when a login was removed.
    pub fn remove_login(&mut self, registry: &str) -> bool {
        self.registries.remove(&normalize(registry)).is_some()
    }
}
