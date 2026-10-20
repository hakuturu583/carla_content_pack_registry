//! HTTP client for the registry API (openapi/openapi.yaml), plus the workflows the CLI
//! builds on it (bucket resolution, ACL edits, parallel pulls). Responses the CLI only
//! displays are kept as `serde_json::Value`.
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::blocking::{Client as Http, RequestBuilder};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::store::{Store, normalize};

/// RFC 3986 unreserved characters stay as-is; everything else in a key segment is encoded.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

pub(crate) const USER_AGENT: &str = concat!("carla-pack/", env!("CARGO_PKG_VERSION"));

/// The one place HTTP clients are configured (registry, storage and GitHub).
pub(crate) fn http_client() -> Result<Http> {
    Ok(Http::builder().user_agent(USER_AGENT).connect_timeout(Duration::from_secs(15)).build()?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectSummary {
    pub key: String,
    pub size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObjectPage {
    objects: Vec<ObjectSummary>,
    next_cursor: Option<String>,
}

/// An ACL entry by GitHub name, as the API accepts it (`PUT /v1/buckets/{id}/acl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AclRef {
    User { login: String },
    Org { login: String },
    Team { org: String, slug: String },
}

impl AclRef {
    /// `team` values look like `ORG/SLUG`.
    pub fn parse(kind: &str, value: &str) -> Result<Self> {
        Ok(match kind {
            "user" => Self::User { login: value.to_string() },
            "org" => Self::Org { login: value.to_string() },
            "team" => match value.split_once('/') {
                Some((org, slug)) if !org.is_empty() && !slug.is_empty() => {
                    Self::Team { org: org.to_string(), slug: slug.to_string() }
                }
                _ => return Err(Error::Usage(format!("teams look like ORG/TEAM-SLUG, got {value:?}"))),
            },
            _ => return Err(Error::Usage(format!("unknown ACL entry type {kind:?}"))),
        })
    }

    /// Converts a stored entry (`{type, id, name}`) back to the name form.
    fn from_entry(e: &Value) -> Option<Self> {
        let name = e["name"].as_str()?;
        Self::parse(e["type"].as_str()?, name).ok()
    }

    /// Identity used to match entries; same rule as the server's dedupe (src/core/registry.ts putAcl).
    fn key(&self) -> String {
        match self {
            Self::User { login } => format!("user:{login}"),
            Self::Org { login } => format!("org:{login}"),
            Self::Team { org, slug } => format!("team:{org}/{slug}"),
        }
        .to_lowercase()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclChange {
    Set,
    Add,
    Remove,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum PullEvent {
    Downloaded { key: String, dest: PathBuf, bytes: u64 },
    Skipped { key: String, dest: PathBuf },
}

pub struct Client {
    base: String,
    token: Option<String>,
    http: Http,
}

impl Client {
    pub fn new(registry_url: &str, token: Option<String>) -> Result<Self> {
        Ok(Self { base: normalize(registry_url), token, http: http_client()? })
    }

    /// Registry and token from the argument, environment and saved login (see store.rs).
    /// With `require_auth`, fails with `NotLoggedIn` when no token is available.
    pub fn from_store(explicit_registry: Option<&str>, require_auth: bool) -> Result<Self> {
        let store = Store::load()?;
        let registry = store.registry(explicit_registry);
        let token = store.token(&registry);
        if require_auth && token.is_none() {
            return Err(Error::NotLoggedIn(registry));
        }
        Self::new(&registry, token)
    }

    /// Same registry and connection pool, different token.
    pub fn with_token(&self, token: String) -> Self {
        Self { base: self.base.clone(), token: Some(token), http: self.http.clone() }
    }

    pub fn registry_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req = self.http.request(method, format!("{}{}", self.base, path)).timeout(Duration::from_secs(60));
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    fn send<T: serde::de::DeserializeOwned>(&self, req: RequestBuilder) -> Result<Option<T>> {
        let res = req.send()?;
        let status = res.status();
        if status == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if status.is_success() {
            return Ok(Some(res.json()?));
        }
        let body: Value = res.json().unwrap_or(Value::Null);
        let err = &body["error"];
        Err(Error::Api {
            status: status.as_u16(),
            code: err["code"].as_str().unwrap_or("http_error").to_string(),
            message: err["message"].as_str().unwrap_or("request failed").to_string(),
        })
    }

    fn json(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut req = self.request(method, path);
        if let Some(b) = body {
            req = req.json(b);
        }
        Ok(self.send(req)?.unwrap_or(Value::Null))
    }

    // ---- endpoints ---------------------------------------------------------------

    pub fn cli_config(&self) -> Result<Value> {
        self.json(Method::GET, "/cli/config", None)
    }

    pub fn me(&self) -> Result<Value> {
        self.json(Method::GET, "/v1/me", None)
    }

    pub fn list_buckets(&self) -> Result<Vec<Value>> {
        Ok(serde_json::from_value(self.json(Method::GET, "/v1/buckets", None)?["buckets"].take())?)
    }

    pub fn get_bucket(&self, id: &str) -> Result<Value> {
        self.json(Method::GET, &bucket_path(id, ""), None)
    }

    pub fn create_bucket(&self, body: &Value) -> Result<Value> {
        self.json(Method::POST, "/v1/buckets", Some(body))
    }

    pub fn update_bucket(&self, id: &str, body: &Value) -> Result<Value> {
        self.json(Method::PATCH, &bucket_path(id, ""), Some(body))
    }

    pub fn delete_bucket(&self, id: &str) -> Result<()> {
        self.json(Method::DELETE, &bucket_path(id, ""), None).map(|_| ())
    }

    pub fn verify_bucket(&self, id: &str) -> Result<Value> {
        self.json(Method::POST, &bucket_path(id, "/verify"), None)
    }

    pub fn put_credentials(&self, id: &str, body: &Value) -> Result<Value> {
        self.json(Method::PUT, &bucket_path(id, "/credentials"), Some(body))
    }

    /// ACL entries (`[{type, id, name}]`).
    pub fn get_acl(&self, id: &str) -> Result<Value> {
        Ok(self.json(Method::GET, &bucket_path(id, "/acl"), None)?["entries"].take())
    }

    /// Replaces, extends or shrinks the ACL; returns the stored entries.
    pub fn update_acl(&self, id: &str, change: AclChange, refs: Vec<AclRef>) -> Result<Value> {
        if change != AclChange::Set && refs.is_empty() {
            return Err(Error::Usage("specify at least one --user, --org or --team".into()));
        }
        let refs = match change {
            AclChange::Set => refs,
            // The server dedupes, so "add" can simply append.
            AclChange::Add => self.current_acl(id)?.into_iter().chain(refs).collect(),
            AclChange::Remove => {
                let drop: Vec<String> = refs.iter().map(AclRef::key).collect();
                self.current_acl(id)?.into_iter().filter(|r| !drop.contains(&r.key())).collect()
            }
        };
        let body = json!({ "entries": refs });
        Ok(self.json(Method::PUT, &bucket_path(id, "/acl"), Some(&body))?["entries"].take())
    }

    fn current_acl(&self, id: &str) -> Result<Vec<AclRef>> {
        Ok(self.get_acl(id)?.as_array().into_iter().flatten().filter_map(AclRef::from_entry).collect())
    }

    /// Every object under `prefix`, following pagination.
    pub fn list_objects(&self, id: &str, prefix: Option<&str>) -> Result<Vec<ObjectSummary>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q: Vec<(&str, &str)> = vec![("limit", "1000")];
            if let Some(p) = prefix.filter(|p| !p.is_empty()) {
                q.push(("prefix", p));
            }
            if let Some(c) = &cursor {
                q.push(("cursor", c));
            }
            let page: ObjectPage = self
                .send(self.request(Method::GET, &bucket_path(id, "/objects")).query(&q))?
                .ok_or_else(|| Error::Usage("empty object listing".into()))?;
            out.extend(page.objects);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }

    /// Downloads one object directly from storage into `dest` (written atomically via `<dest>.part`).
    /// Returns the number of bytes written.
    pub fn download(&self, id: &str, key: &str, dest: &Path) -> Result<u64> {
        let presigned =
            self.json(Method::GET, &bucket_path(id, &format!("/objects/{}/download", encode_key(key))), None)?;
        let url = presigned["url"].as_str().ok_or_else(|| Error::Usage("registry returned no download URL".into()))?;
        // The presigned URL carries its own authorization; never send the registry token to storage.
        let mut res = self.http.get(url).timeout(Duration::from_secs(3600)).send()?;
        if !res.status().is_success() {
            return Err(Error::Api {
                status: res.status().as_u16(),
                code: "storage_error".into(),
                message: format!("storage refused the download of {key}"),
            });
        }
        let expected = res.content_length();
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let part = part_path(dest);
        let written = (|| -> Result<u64> {
            let mut file = BufWriter::with_capacity(1 << 20, fs::File::create(&part)?);
            let n = io::copy(&mut res, &mut file)?;
            file.flush()?;
            if let Some(len) = expected.filter(|len| *len != n) {
                return Err(Error::Usage(format!("download of {key} was truncated ({n} of {len} bytes)")));
            }
            Ok(n)
        })();
        match written {
            Ok(n) => {
                fs::rename(&part, dest)?;
                Ok(n)
            }
            Err(e) => {
                let _ = fs::remove_file(&part);
                Err(e)
            }
        }
    }

    /// Downloads exact keys and everything under targets ending in "/" (no targets: the
    /// whole bucket) into `out_dir`, `jobs` objects at a time. Existing files are skipped
    /// unless `force`. Stops scheduling new downloads after the first error.
    pub fn pull(
        &self,
        id: &str,
        targets: &[String],
        out_dir: &Path,
        force: bool,
        jobs: usize,
        on_event: impl Fn(PullEvent) + Sync,
    ) -> Result<u64> {
        let all = [String::new()];
        let mut keys = Vec::new();
        for t in if targets.is_empty() { &all[..] } else { targets } {
            if t.is_empty() || t.ends_with('/') {
                keys.extend(self.list_objects(id, Some(t))?.into_iter().map(|o| o.key));
            } else {
                keys.push(t.clone());
            }
        }
        if keys.is_empty() {
            return Err(Error::Usage("nothing to download".into()));
        }
        // Map every key first so a hostile key fails before anything is written.
        let work: Vec<(String, PathBuf)> =
            keys.into_iter().map(|k| safe_dest(out_dir, &k).map(|d| (k, d))).collect::<Result<_>>()?;

        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let first_error: Mutex<Option<Error>> = Mutex::new(None);
        let total = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..jobs.clamp(1, 16).min(work.len()) {
                s.spawn(|| {
                    while !failed.load(Ordering::Relaxed) {
                        let Some((key, dest)) = work.get(next.fetch_add(1, Ordering::Relaxed)) else { return };
                        if dest.exists() && !force {
                            on_event(PullEvent::Skipped { key: key.clone(), dest: dest.clone() });
                            continue;
                        }
                        match self.download(id, key, dest) {
                            Ok(bytes) => {
                                total.fetch_add(bytes as usize, Ordering::Relaxed);
                                on_event(PullEvent::Downloaded { key: key.clone(), dest: dest.clone(), bytes });
                            }
                            Err(e) => {
                                failed.store(true, Ordering::Relaxed);
                                first_error.lock().unwrap().get_or_insert(e);
                            }
                        }
                    }
                });
            }
        });
        match first_error.into_inner().unwrap() {
            Some(e) => Err(e),
            None => Ok(total.into_inner() as u64),
        }
    }

    /// Resolves `owner/name`, `name` or a bucket UUID to the full bucket view.
    pub fn resolve_bucket(&self, reference: &str) -> Result<Value> {
        if is_uuid(reference) {
            return self.get_bucket(reference);
        }
        let (owner, name) = match reference.split_once('/') {
            Some((o, n)) => (Some(o), n),
            None => (None, reference),
        };
        let mut matches: Vec<Value> = self
            .list_buckets()?
            .into_iter()
            .filter(|b| {
                b["name"] == name
                    && owner.is_none_or(|o| b["owner"]["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(o)))
            })
            .collect();
        match matches.len() {
            0 => Err(Error::Usage(format!("no readable bucket named {reference:?}"))),
            1 => Ok(matches.remove(0)),
            _ => Err(Error::Usage(format!(
                "{reference:?} is ambiguous; use one of: {}",
                matches.iter().map(display_ref).collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}

/// `owner/name` as shown to users.
pub fn display_ref(bucket: &Value) -> String {
    let owner =
        bucket["owner"]["login"].as_str().map(str::to_string).unwrap_or_else(|| bucket["owner"]["id"].to_string());
    format!("{owner}/{}", bucket["name"].as_str().unwrap_or("?"))
}

/// Maps an object key below `out_dir`, refusing keys that would escape it.
pub fn safe_dest(out_dir: &Path, key: &str) -> Result<PathBuf> {
    let escape = || Error::Usage(format!("refusing to write object key {key:?} outside the output directory"));
    let mut dest = out_dir.to_path_buf();
    let mut any = false;
    for c in Path::new(key).components() {
        match c {
            Component::Normal(part) => {
                dest.push(part);
                any = true;
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => return Err(escape()),
        }
    }
    if any { Ok(dest) } else { Err(escape()) }
}

fn bucket_path(id: &str, suffix: &str) -> String {
    format!("/v1/buckets/{}{}", utf8_percent_encode(id, SEGMENT), suffix)
}

/// Encodes each key segment and keeps "/" (the form the API documents for keys).
fn encode_key(key: &str) -> String {
    key.split('/').map(|s| utf8_percent_encode(s, SEGMENT).to_string()).collect::<Vec<_>>().join("/")
}

fn part_path(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars()
            .enumerate()
            .all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_keys_per_segment() {
        assert_eq!(encode_key("maps/Town 10 (HD)!.tar"), "maps/Town%2010%20%28HD%29%21.tar");
        assert_eq!(encode_key("日本/x+y"), "%E6%97%A5%E6%9C%AC/x%2By");
    }

    #[test]
    fn detects_uuids() {
        assert!(is_uuid("5d93316f-ced0-4419-836f-813b9d450715"));
        assert!(!is_uuid("alice/maps"));
    }

    #[test]
    fn safe_dest_stays_inside_out_dir() {
        let out = Path::new("/tmp/out");
        assert_eq!(safe_dest(out, "maps/a.tar").unwrap(), out.join("maps/a.tar"));
        assert_eq!(safe_dest(out, "/maps/./a.tar").unwrap(), out.join("maps/a.tar"));
        for bad in ["../x", "maps/../../x", "maps/../x", "/", ".", ""] {
            assert!(safe_dest(out, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn acl_refs_parse_and_match_case_insensitively() {
        assert_eq!(
            AclRef::parse("team", "Carla/Maps").unwrap(),
            AclRef::Team { org: "Carla".into(), slug: "Maps".into() }
        );
        assert!(AclRef::parse("team", "carla").is_err());
        assert_eq!(AclRef::parse("user", "Bob").unwrap().key(), AclRef::parse("user", "bob").unwrap().key());
        let stored = json!({ "type": "team", "id": 6001, "name": "carla-sim/maps" });
        assert_eq!(AclRef::from_entry(&stored), AclRef::parse("team", "carla-sim/maps").ok());
    }
}
