//! Single-object requests to an S3-compatible bucket (AWS S3, Cloudflare R2, MinIO, SeaweedFS).
use std::fs;
use std::path::Path;
use std::time::Duration;

use reqwest::Url;
use reqwest::blocking::{Body, Client as Http, RequestBuilder, Response};

use crate::error::{Error, Result};
use crate::manifest::{Addressing, StorageConfig};
use crate::sigv4::{self, Credentials, UNSIGNED_PAYLOAD};
use crate::time;

/// Largest object a single PUT may create (S3 and R2 limit).
pub const MAX_SINGLE_PUT: u64 = 5 * 1024 * 1024 * 1024;

pub struct Bucket<'a> {
    pub config: &'a StorageConfig,
    /// `None`: anonymous (public bucket).
    pub credentials: Option<&'a Credentials>,
}

impl Bucket<'_> {
    /// URL, Host header value and canonical URI of `object_key`.
    fn locate(&self, object_key: &str) -> Result<(Url, String, String)> {
        let endpoint =
            Url::parse(&self.config.endpoint).map_err(|e| Error::Manifest(format!("storage.endpoint: {e}")))?;
        let host = endpoint.host_str().ok_or_else(|| Error::Manifest("storage.endpoint has no host".into()))?;
        let (host, path) = match self.config.addressing {
            Addressing::Path => (host.to_string(), format!("/{}/{object_key}", self.config.bucket)),
            Addressing::Virtual => (format!("{}.{host}", self.config.bucket), format!("/{object_key}")),
        };
        let host = match endpoint.port() {
            Some(port) => format!("{host}:{port}"),
            None => host,
        };
        let canonical_uri = sigv4::encode_path(&path);
        let url = Url::parse(&format!("{}://{host}{canonical_uri}", endpoint.scheme()))
            .map_err(|e| Error::Manifest(format!("cannot build an object URL: {e}")))?;
        Ok((url, host, canonical_uri))
    }

    fn request(&self, http: &Http, method: reqwest::Method, object_key: &str) -> Result<RequestBuilder> {
        let (url, host, canonical_uri) = self.locate(object_key)?;
        let mut req = http.request(method.clone(), url);
        if let Some(creds) = self.credentials {
            let signing = sigv4::Request {
                method: method.as_str(),
                host: &host,
                canonical_uri: &canonical_uri,
                canonical_query: "",
                headers: &[],
                payload_hash: UNSIGNED_PAYLOAD,
            };
            for (name, value) in sigv4::sign(&signing, creds, &self.config.region, &time::amz_date(time::now())) {
                req = req.header(name, value);
            }
        }
        Ok(req)
    }

    /// GET; the body is streamed by the caller.
    pub fn get(&self, http: &Http, object_key: &str) -> Result<Response> {
        let res =
            self.request(http, reqwest::Method::GET, object_key)?.timeout(Duration::from_secs(6 * 3600)).send()?;
        check(res, object_key)
    }

    /// Size of the object, `None` when it does not exist.
    pub fn head(&self, http: &Http, object_key: &str) -> Result<Option<u64>> {
        let res = self.request(http, reqwest::Method::HEAD, object_key)?.timeout(Duration::from_secs(60)).send()?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let res = check(res, object_key)?;
        Ok(res
            .content_length()
            .or_else(|| res.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok())))
    }

    /// Uploads `file` as `object_key` with one PUT.
    pub fn put_file(&self, http: &Http, object_key: &str, file: &Path) -> Result<()> {
        let size = fs::metadata(file)?.len();
        if size > MAX_SINGLE_PUT {
            return Err(Error::Usage(format!(
                "{} is larger than 5 GiB, the single-upload limit; upload it with `aws s3 cp` / rclone and \
                 publish with --no-upload",
                file.display()
            )));
        }
        let body = Body::sized(fs::File::open(file)?, size);
        let res = self
            .request(http, reqwest::Method::PUT, object_key)?
            .header("content-type", "application/gzip")
            .body(body)
            .timeout(Duration::from_secs(6 * 3600))
            .send()?;
        check(res, object_key).map(drop)
    }
}

fn check(res: Response, object_key: &str) -> Result<Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let body = res.text().unwrap_or_default();
    let code = between(&body, "<Code>", "</Code>").unwrap_or("");
    let message = between(&body, "<Message>", "</Message>").unwrap_or("");
    let hint = match status.as_u16() {
        403 => {
            " (the storage credentials were refused: they may have been rotated or revoked; retry, or ask the pack owner)"
        }
        404 => " (no such object or bucket)",
        _ => "",
    };
    Err(Error::Storage {
        status: status.as_u16(),
        message: format!(
            "{object_key}: {}{hint}",
            [code, message].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(": ")
        ),
    })
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = s.find(start)? + start.len();
    let to = s[from..].find(end)? + from;
    Some(&s[from..to])
}
