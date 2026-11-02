//! Object requests to an S3-compatible bucket (AWS S3, Cloudflare R2, MinIO, SeaweedFS):
//! GET (optionally from an offset, to resume), HEAD, and uploads -- one PUT for small
//! files, a multipart upload for anything larger than one part.
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use reqwest::Url;
use reqwest::blocking::{Body, Client as Http, RequestBuilder, Response};

use crate::error::{Error, Result};
use crate::manifest::{Addressing, StorageConfig};
use crate::sigv4::{self, Credentials, UNSIGNED_PAYLOAD};
use crate::time;

const MIB: u64 = 1024 * 1024;

/// Default multipart part size; files up to one part go up in a single PUT.
pub const DEFAULT_PART_SIZE: u64 = 64 * MIB;
/// S3 / R2 limits: parts of at least 5 MiB (except the last), at most 10,000 parts.
const MIN_PART_SIZE: u64 = 5 * MIB;
const MAX_PARTS: u64 = 10_000;
/// Attempts per request of an upload (network errors and 5xx answers are retried).
const UPLOAD_ATTEMPTS: u32 = 3;

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
        self.request_with(http, method, object_key, &[], &[])
    }

    /// A signed request with query parameters and extra signed headers (lower-case names).
    fn request_with(
        &self,
        http: &Http,
        method: reqwest::Method,
        object_key: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Result<RequestBuilder> {
        let (mut url, host, canonical_uri) = self.locate(object_key)?;
        let mut pairs: Vec<(String, String)> = query.iter().map(|(k, v)| (encode_query(k), encode_query(v))).collect();
        pairs.sort();
        let canonical_query = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
        if !canonical_query.is_empty() {
            url.set_query(Some(&canonical_query));
        }
        let mut req = http.request(method.clone(), url);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        if let Some(creds) = self.credentials {
            let signing = sigv4::Request {
                method: method.as_str(),
                host: &host,
                canonical_uri: &canonical_uri,
                canonical_query: &canonical_query,
                headers,
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

    /// GET from byte `offset` on (to resume a broken download). `None` when the storage
    /// ignored the range and sent the whole object again.
    pub fn get_from(&self, http: &Http, object_key: &str, offset: u64) -> Result<Option<Response>> {
        let range = format!("bytes={offset}-");
        let res = self
            .request_with(http, reqwest::Method::GET, object_key, &[], &[("range", &range)])?
            .timeout(Duration::from_secs(6 * 3600))
            .send()?;
        let res = check(res, object_key)?;
        Ok((res.status() == reqwest::StatusCode::PARTIAL_CONTENT).then_some(res))
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

    /// Uploads `file` as `object_key`: one PUT up to `part_size`, a multipart upload in
    /// parts of `part_size` (raised to stay within 10,000 parts) above it. A failed
    /// multipart upload is aborted, so no partial object or stored parts are left behind.
    pub fn put_file(&self, http: &Http, object_key: &str, file: &Path, part_size: u64) -> Result<()> {
        let size = fs::metadata(file)?.len();
        let part_size = part_size.max(MIN_PART_SIZE).max(size.div_ceil(MAX_PARTS));
        if size <= part_size {
            return retry(|| {
                let body = Body::sized(fs::File::open(file)?, size);
                let res = self
                    .request(http, reqwest::Method::PUT, object_key)?
                    .header("content-type", "application/gzip")
                    .body(body)
                    .timeout(Duration::from_secs(6 * 3600))
                    .send()?;
                check(res, object_key).map(drop)
            });
        }

        let res = retry(|| {
            let res = self
                .request_with(http, reqwest::Method::POST, object_key, &[("uploads", "")], &[])?
                .header("content-type", "application/gzip")
                .timeout(Duration::from_secs(120))
                .send()?;
            Ok(check(res, object_key)?.text()?)
        })?;
        let upload_id = between(&res, "<UploadId>", "</UploadId>")
            .ok_or_else(|| Error::Storage { status: 200, message: format!("{object_key}: no UploadId in {res:?}") })?
            .to_string();

        let uploaded = self.upload_parts(http, object_key, file, size, part_size, &upload_id);
        let completed = uploaded.and_then(|etags| {
            let mut xml = String::from("<CompleteMultipartUpload>");
            for (i, etag) in etags.iter().enumerate() {
                xml.push_str(&format!(
                    "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                    i + 1,
                    xml_escape(etag)
                ));
            }
            xml.push_str("</CompleteMultipartUpload>");
            retry(|| {
                let res = self
                    .request_with(http, reqwest::Method::POST, object_key, &[("uploadId", &upload_id)], &[])?
                    .header("content-type", "application/xml")
                    .body(xml.clone())
                    .timeout(Duration::from_secs(600))
                    .send()?;
                // S3 can answer 200 and still report an error in the body.
                let body = check(res, object_key)?.text()?;
                match between(&body, "<Code>", "</Code>") {
                    Some(code) if body.contains("<Error>") => Err(Error::Storage {
                        status: 200,
                        message: format!("{object_key}: completing the upload failed: {code}"),
                    }),
                    _ => Ok(()),
                }
            })
        });
        if completed.is_err() {
            let _ = self
                .request_with(http, reqwest::Method::DELETE, object_key, &[("uploadId", &upload_id)], &[])
                .and_then(|r| Ok(r.timeout(Duration::from_secs(60)).send()?));
        }
        completed
    }

    /// Uploads every part; returns their ETags in order.
    fn upload_parts(
        &self,
        http: &Http,
        object_key: &str,
        file: &Path,
        size: u64,
        part_size: u64,
        upload_id: &str,
    ) -> Result<Vec<String>> {
        let mut etags = Vec::new();
        let mut offset = 0;
        while offset < size {
            let len = part_size.min(size - offset);
            let number = (etags.len() + 1).to_string();
            let etag = retry(|| {
                let mut f = fs::File::open(file)?;
                f.seek(SeekFrom::Start(offset))?;
                let res = self
                    .request_with(
                        http,
                        reqwest::Method::PUT,
                        object_key,
                        &[("partNumber", &number), ("uploadId", upload_id)],
                        &[],
                    )?
                    .body(Body::sized(f.take(len), len))
                    .timeout(Duration::from_secs(3600))
                    .send()?;
                let res = check(res, object_key)?;
                res.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string).ok_or_else(|| {
                    Error::Storage {
                        status: 200,
                        message: format!("{object_key}: part {number} came back without an ETag"),
                    }
                })
            })?;
            etags.push(etag);
            offset += len;
        }
        Ok(etags)
    }
}

/// Runs `f` up to UPLOAD_ATTEMPTS times while it fails with a network error or a 5xx answer.
fn retry<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut attempt = 1;
    loop {
        match f() {
            Err(Error::Http(_) | Error::Storage { status: 500..=599, .. }) if attempt < UPLOAD_ATTEMPTS => {
                std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// URI-encodes a query key or value for SigV4 (everything but unreserved characters).
fn encode_query(s: &str) -> String {
    sigv4::encode_path(s).replace('/', "%2F")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
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
