//! HTTP client for the registry API (openapi/openapi.yaml). Responses are kept as
//! `serde_json::Value` where the CLI only displays them.
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::blocking::{Client as Http, RequestBuilder, Response};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};

/// RFC 3986 unreserved characters stay as-is; everything else in a key segment is encoded.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

const USER_AGENT: &str = concat!("carla-pack/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Deserialize)]
pub struct Owner {
    pub id: u64,
    pub login: Option<String>,
}

/// The subset of a bucket the CLI needs for resolution; full views stay as JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct BucketRef {
    pub id: String,
    pub name: String,
    pub owner: Owner,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObjectSummary {
    pub key: String,
    pub size: u64,
}

pub struct Client {
    base: String,
    token: Option<String>,
    http: Http,
}

impl Client {
    pub fn new(registry_url: &str, token: Option<String>) -> Result<Self> {
        let http = Http::builder().user_agent(USER_AGENT).connect_timeout(Duration::from_secs(15)).build()?;
        Ok(Self { base: registry_url.trim_end_matches('/').to_string(), token, http })
    }

    pub fn registry_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let mut req = self.http.request(method, format!("{}{}", self.base, path)).timeout(Duration::from_secs(60));
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        Ok(req)
    }

    fn send(&self, req: RequestBuilder) -> Result<Response> {
        let res = req.send()?;
        if res.status().is_success() || res.status() == StatusCode::FOUND {
            return Ok(res);
        }
        let status = res.status().as_u16();
        let body: Value = res.json().unwrap_or(Value::Null);
        let err = &body["error"];
        Err(Error::Api {
            status,
            code: err["code"].as_str().unwrap_or("http_error").to_string(),
            message: err["message"].as_str().unwrap_or("request failed").to_string(),
        })
    }

    fn json(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut req = self.request(method, path)?;
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = self.send(req)?;
        if res.status() == StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        Ok(res.json()?)
    }

    // ---- public endpoints ------------------------------------------------------

    pub fn cli_config(&self) -> Result<Value> {
        self.json(Method::GET, "/cli/config", None)
    }

    // ---- authenticated endpoints -------------------------------------------------

    pub fn me(&self) -> Result<Value> {
        self.json(Method::GET, "/v1/me", None)
    }

    pub fn list_buckets(&self) -> Result<Value> {
        self.json(Method::GET, "/v1/buckets", None)
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

    pub fn get_acl(&self, id: &str) -> Result<Value> {
        self.json(Method::GET, &bucket_path(id, "/acl"), None)
    }

    pub fn put_acl(&self, id: &str, entries: &Value) -> Result<Value> {
        self.json(Method::PUT, &bucket_path(id, "/acl"), Some(&json!({ "entries": entries })))
    }

    /// One page of objects.
    pub fn list_objects(&self, id: &str, prefix: Option<&str>, cursor: Option<&str>, limit: u32) -> Result<Value> {
        let mut q: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(p) = prefix.filter(|p| !p.is_empty()) {
            q.push(("prefix", p.to_string()));
        }
        if let Some(c) = cursor {
            q.push(("cursor", c.to_string()));
        }
        let res = self.send(self.request(Method::GET, &bucket_path(id, "/objects"))?.query(&q))?;
        Ok(res.json()?)
    }

    /// Every object under `prefix`, following pagination.
    pub fn list_all_objects(&self, id: &str, prefix: Option<&str>) -> Result<Vec<ObjectSummary>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self.list_objects(id, prefix, cursor.as_deref(), 1000)?;
            out.extend(serde_json::from_value::<Vec<ObjectSummary>>(page["objects"].clone())?);
            match page["nextCursor"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => return Ok(out),
            }
        }
    }

    /// Presigned download URL (JSON: url, method, expiresAt).
    pub fn download_url(&self, id: &str, key: &str) -> Result<Value> {
        self.json(Method::GET, &bucket_path(id, &format!("/objects/{}/download", encode_key(key))), None)
    }

    /// Downloads one object directly from storage into `dest` (written atomically via `<dest>.part`).
    /// Returns the number of bytes written.
    pub fn download(&self, id: &str, key: &str, dest: &Path) -> Result<u64> {
        let presigned = self.download_url(id, key)?;
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
            let mut file = fs::File::create(&part)?;
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

    /// Resolves `owner/name`, `name` or a bucket UUID to a bucket the caller can read.
    pub fn resolve_bucket(&self, reference: &str) -> Result<BucketRef> {
        if is_uuid(reference) {
            return Ok(serde_json::from_value(self.get_bucket(reference)?)?);
        }
        let (owner, name) = match reference.split_once('/') {
            Some((o, n)) => (Some(o), n),
            None => (None, reference),
        };
        let all: Vec<BucketRef> = serde_json::from_value(self.list_buckets()?["buckets"].clone())?;
        let matches: Vec<BucketRef> = all
            .into_iter()
            .filter(|b| {
                b.name == name
                    && owner.is_none_or(|o| b.owner.login.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(o)))
            })
            .collect();
        match matches.len() {
            0 => Err(Error::Usage(format!("no readable bucket named {reference:?}"))),
            1 => Ok(matches.into_iter().next().unwrap()),
            _ => Err(Error::Usage(format!(
                "{reference:?} is ambiguous; use one of: {}",
                matches
                    .iter()
                    .map(|b| format!("{}/{}", b.owner.login.as_deref().unwrap_or("?"), b.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

fn bucket_path(id: &str, suffix: &str) -> String {
    format!("/v1/buckets/{}{}", utf8_percent_encode(id, SEGMENT), suffix)
}

/// Encodes each key segment and keeps "/" (the form the API documents for keys).
pub fn encode_key(key: &str) -> String {
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
}
