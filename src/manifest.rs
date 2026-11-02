//! `carla-packs.toml`: the file at the root of a pack repository. It says where the packs
//! are stored and lists every published pack with its SHA-256. Reading it through the
//! GitHub API is the access check: GitHub answers 404 to anyone without read access.
//!
//! ```toml
//! schema = 1
//!
//! [storage]
//! endpoint = "https://<account>.r2.cloudflarestorage.com"
//! region = "auto"
//! bucket = "carla-packs"
//! prefix = "packs/"                 # optional
//! credentials = "github-release"    # or "anonymous" for a public bucket
//!
//! [[pack]]
//! name = "Town01Pack"
//! version = "1.0.0"
//! base_release = "carla-0.10.0-Linux"
//! platform = "Linux"
//! key = "Town01Pack-1.0.0-carla-0.10.0-Linux.tar.gz"
//! sha256 = "…"
//! size = 123456
//! maps = ["Town01_Opt"]
//! ```
use std::cmp::Ordering;
use std::collections::HashSet;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::pack::{is_clean_relpath, is_pack_name};

pub const MANIFEST_FILE: &str = "carla-packs.toml";
pub const SCHEMA: u32 = 1;
/// Release tag and asset name the credential workflow publishes to.
pub const DEFAULT_CREDENTIALS_TAG: &str = "carla-pack-credentials";
pub const CREDENTIALS_ASSET: &str = "credentials.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub storage: StorageConfig,
    #[serde(default, rename = "pack")]
    pub packs: Vec<PackEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialMode {
    /// Read-only credentials published as a release asset of the (private) pack repository.
    GithubRelease,
    /// A public bucket: unsigned requests.
    Anonymous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Addressing {
    /// `https://endpoint/bucket/key` (R2, MinIO, SeaweedFS, S3).
    #[default]
    Path,
    /// `https://bucket.endpoint/key`.
    Virtual,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub endpoint: String,
    #[serde(default = "default_region")]
    pub region: String,
    pub bucket: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub addressing: Addressing,
    pub credentials: CredentialMode,
    #[serde(default = "default_tag")]
    pub credentials_tag: String,
}

fn default_region() -> String {
    "auto".into()
}

fn default_tag() -> String {
    DEFAULT_CREDENTIALS_TAG.into()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackEntry {
    pub name: String,
    pub version: String,
    pub base_release: String,
    pub platform: String,
    /// Object key, relative to `storage.prefix`.
    pub key: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub maps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Withdrawn: never selected unless pinned by SHA-256.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub yanked: bool,
}

impl Manifest {
    pub fn parse(text: &str, origin: &str) -> Result<Self> {
        let m: Self = toml::from_str(text).map_err(|e| Error::Manifest(format!("{origin}: {e}")))?;
        m.validate().map_err(|e| Error::Manifest(format!("{origin}: {e}")))?;
        Ok(m)
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!("unsupported schema {} (this client reads schema {SCHEMA})", self.schema));
        }
        self.storage.validate()?;
        let mut seen = HashSet::new();
        for p in &self.packs {
            p.validate()?;
            if !seen.insert((&p.name, &p.version, &p.base_release, &p.platform)) {
                return Err(format!("{} {} ({}, {}) is listed twice", p.name, p.version, p.base_release, p.platform));
            }
        }
        Ok(())
    }
}

impl StorageConfig {
    fn validate(&self) -> std::result::Result<(), String> {
        let url = Url::parse(&self.endpoint).map_err(|e| format!("storage.endpoint: {e}"))?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err("storage.endpoint must be https:// (http:// only for localhost)".into());
        }
        if url.path() != "/" || url.query().is_some() || !url.username().is_empty() || url.password().is_some() {
            return Err(
                "storage.endpoint must be a bare origin such as https://<account>.r2.cloudflarestorage.com".into()
            );
        }
        let b = &self.bucket;
        if b.len() < 3
            || b.len() > 63
            || !b.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        {
            return Err(format!("storage.bucket {b:?} is not a valid bucket name"));
        }
        if !self.region.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') || self.region.is_empty() {
            return Err(format!("storage.region {:?} is not a valid region", self.region));
        }
        if !self.prefix.is_empty()
            && !(self.prefix.ends_with('/') && is_clean_relpath(&self.prefix[..self.prefix.len() - 1]))
        {
            return Err(format!("storage.prefix {:?} must be a relative path ending in /", self.prefix));
        }
        if self.credentials_tag.is_empty() || self.credentials_tag.contains(['/', ' ']) {
            return Err(format!("storage.credentials_tag {:?} is not a valid tag", self.credentials_tag));
        }
        Ok(())
    }

    /// Full object key for a pack's key.
    pub fn object_key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

impl PackEntry {
    fn validate(&self) -> std::result::Result<(), String> {
        if !is_pack_name(&self.name) {
            return Err(format!("invalid pack name {:?}", self.name));
        }
        for (field, value) in
            [("version", &self.version), ("base_release", &self.base_release), ("platform", &self.platform)]
        {
            if value.is_empty() || value.contains(['/', '@', ' ']) {
                return Err(format!("{}: invalid {field} {value:?}", self.name));
            }
        }
        if !is_clean_relpath(&self.key) {
            return Err(format!("{} {}: invalid key {:?}", self.name, self.version, self.key));
        }
        if !is_sha256(&self.sha256) {
            return Err(format!("{} {}: sha256 must be 64 lower-case hex digits", self.name, self.version));
        }
        Ok(())
    }
}

pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// `OWNER/REPO`: a GitHub repository name.
pub fn validate_repo(repo: &str) -> Result<()> {
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'));
    match repo.split_once('/') {
        Some((owner, name)) if ok(owner) && ok(name) && name != "." && name != ".." => Ok(()),
        _ => Err(Error::Usage(format!("{repo:?} is not a GitHub repository (expected OWNER/REPO)"))),
    }
}

/// `OWNER/REPO/PACK[@VERSION]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRef {
    pub repo: String,
    pub name: String,
    pub version: Option<String>,
}

impl PackRef {
    pub fn parse(text: &str) -> Result<Self> {
        let bad = || Error::Usage(format!("{text:?} is not a pack reference (expected OWNER/REPO/PACK[@VERSION])"));
        let (path, version) = match text.trim().split_once('@') {
            Some((p, v)) if !v.is_empty() => (p, Some(v.to_string())),
            Some(_) => return Err(bad()),
            None => (text.trim(), None),
        };
        let mut parts = path.split('/');
        let (Some(owner), Some(repo), Some(name), None) = (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let repo = format!("{owner}/{repo}");
        validate_repo(&repo).map_err(|_| bad())?;
        if !is_pack_name(name) {
            return Err(bad());
        }
        Ok(Self { repo, name: name.to_string(), version })
    }
}

impl std::fmt::Display for PackRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.repo, self.name)?;
        if let Some(v) = &self.version {
            write!(f, "@{v}")?;
        }
        Ok(())
    }
}

/// Narrows the releases a reference or map name can select.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub base_release: Option<String>,
    pub platform: Option<String>,
    /// Pins one exact artifact (a lock file); also selects yanked releases.
    pub sha256: Option<String>,
}

impl Filter {
    pub fn matches(&self, p: &PackEntry) -> bool {
        self.base_release.as_ref().is_none_or(|b| *b == p.base_release)
            && self.platform.as_ref().is_none_or(|x| x.eq_ignore_ascii_case(&p.platform))
            && match &self.sha256 {
                Some(s) => s.eq_ignore_ascii_case(&p.sha256),
                None => !p.yanked,
            }
    }
}

/// Picks the newest of `candidates`; an error when the newest version exists for several
/// base releases or platforms and the filter does not say which.
pub fn select<T>(candidates: Vec<T>, entry: impl Fn(&T) -> &PackEntry, what: &str) -> Result<T> {
    let mut candidates = candidates;
    candidates.sort_by(|a, b| compare_versions(&entry(b).version, &entry(a).version));
    let Some(first) = candidates.first() else {
        return Err(Error::NotFound(format!("no release of {what} matches")));
    };
    let newest = entry(first).version.clone();
    let tied: Vec<String> = candidates
        .iter()
        .filter(|c| compare_versions(&entry(c).version, &newest) == Ordering::Equal)
        .map(|c| format!("{} {} ({}, {})", entry(c).name, entry(c).version, entry(c).base_release, entry(c).platform))
        .collect();
    if tied.len() > 1 {
        return Err(Error::Usage(format!(
            "{what} is ambiguous: {}; choose with base_release / platform",
            tied.join(", ")
        )));
    }
    Ok(candidates.swap_remove(0))
}

/// Compares versions chunk by chunk (`1.10.0` > `1.9.2`); numeric chunks compare as numbers.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let split = |s: &str| s.split(['.', '-', '+', '_']).map(str::to_string).collect::<Vec<_>>();
    let (a, b) = (split(a), split(b));
    for (x, y) in a.iter().zip(&b) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Greater, // 1.0.0 > 1.0.0-rc1
            (Err(_), Ok(_)) => Ordering::Less,
            _ => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // One is a prefix of the other: `1.0.1` > `1.0`, but `1.0.0` > `1.0.0-rc1`.
    let tail_is_number = |v: &[String]| v.get(a.len().min(b.len())).is_some_and(|c| c.parse::<u64>().is_ok());
    match a.len().cmp(&b.len()) {
        Ordering::Greater if !tail_is_number(&a) => Ordering::Less,
        Ordering::Less if !tail_is_number(&b) => Ordering::Greater,
        ord => ord,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest(extra: &str) -> String {
        format!(
            "schema = 1\n[storage]\nendpoint = \"https://acc.r2.cloudflarestorage.com\"\nbucket = \"packs\"\n\
             credentials = \"github-release\"\n{extra}"
        )
    }

    fn pack(version: &str, base: &str) -> String {
        format!(
            "[[pack]]\nname = \"Town01Pack\"\nversion = \"{version}\"\nbase_release = \"{base}\"\nplatform = \"Linux\"\n\
             key = \"Town01Pack-{version}.tar.gz\"\nsha256 = \"{SHA}\"\nsize = 1\nmaps = [\"Town01\"]\n"
        )
    }

    #[test]
    fn parses_and_validates() {
        let m = Manifest::parse(&manifest(&pack("1.0.0", "carla-0.10.0-Linux")), "t").unwrap();
        assert_eq!(m.storage.region, "auto");
        assert_eq!(m.storage.credentials_tag, DEFAULT_CREDENTIALS_TAG);
        assert_eq!(m.packs[0].maps, ["Town01"]);

        let dup = manifest(&format!("{}{}", pack("1.0.0", "r"), pack("1.0.0", "r")));
        assert!(Manifest::parse(&dup, "t").unwrap_err().to_string().contains("twice"));
        for bad in [
            manifest("").replace("https://acc.r2.cloudflarestorage.com", "http://example.com"),
            manifest("").replace("https://acc.r2.cloudflarestorage.com", "https://x.com/path"),
            manifest("").replace("packs", "Bad_Bucket"),
            manifest("unknown = 1\n"),
            manifest(&pack("1.0.0", "r").replace(SHA, "abc")),
            manifest(&pack("1.0.0", "r").replace("Town01Pack-1.0.0.tar.gz", "../x")),
            manifest("").replace("schema = 1", "schema = 2"),
        ] {
            assert!(Manifest::parse(&bad, "t").is_err(), "{bad}");
        }
        assert!(
            Manifest::parse(
                &manifest("").replace("https://acc.r2.cloudflarestorage.com", "http://127.0.0.1:8333"),
                "t"
            )
            .is_ok()
        );
    }

    #[test]
    fn parses_references() {
        let r = PackRef::parse("tier4/carla-packs/Town01Pack@1.2.0").unwrap();
        assert_eq!(
            (r.repo.as_str(), r.name.as_str(), r.version.as_deref()),
            ("tier4/carla-packs", "Town01Pack", Some("1.2.0"))
        );
        assert_eq!(r.to_string(), "tier4/carla-packs/Town01Pack@1.2.0");
        assert_eq!(PackRef::parse("o/r/P").unwrap().version, None);
        for bad in ["o/r", "o/r/P@", "o/r/P/x", "o/../P", "o/r/1P", "o r/x/P"] {
            assert!(PackRef::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn selects_the_newest_unambiguous_release() {
        let m = Manifest::parse(
            &manifest(&format!(
                "{}{}{}{}",
                pack("1.9.2", "a"),
                pack("1.10.0", "a"),
                pack("1.10.0", "b"),
                pack("2.0.0-rc1", "a")
            )),
            "t",
        )
        .unwrap();
        let pick = |f: Filter| select(m.packs.iter().filter(|p| f.matches(p)).collect(), |p| *p, "Town01Pack");
        assert_eq!(pick(Filter::default()).unwrap().version, "2.0.0-rc1");
        assert_eq!(compare_versions("2.0.0-rc1", "1.10.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc1"), Ordering::Greater);
        let b = pick(Filter { base_release: Some("b".into()), ..Default::default() }).unwrap();
        assert_eq!(b.version, "1.10.0");
        assert!(matches!(
            pick(Filter { base_release: Some("zzz".into()), ..Default::default() }),
            Err(Error::NotFound(_))
        ));

        let tied = Manifest::parse(&manifest(&format!("{}{}", pack("1.0.0", "a"), pack("1.0.0", "b"))), "t").unwrap();
        let err = select(tied.packs.iter().collect(), |p| *p, "Town01Pack").unwrap_err().to_string();
        assert!(err.contains("ambiguous") && err.contains("(a, Linux)") && err.contains("(b, Linux)"), "{err}");
    }
}
