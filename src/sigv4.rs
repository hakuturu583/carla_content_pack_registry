//! AWS Signature Version 4 for single S3 requests, header-signed with `UNSIGNED-PAYLOAD`
//! (the body is not hashed up front, so multi-gigabyte packs stream). Works with AWS S3,
//! Cloudflare R2, MinIO and SeaweedFS.
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// S3 credentials. Debug output never shows the secret or the session token.
#[derive(Clone, Deserialize)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("access_key_id", &self.access_key_id).finish_non_exhaustive()
    }
}

impl Credentials {
    /// Write credentials for `carla-pack publish`: `CARLA_PACK_UPLOAD_ACCESS_KEY_ID` /
    /// `_SECRET_ACCESS_KEY` / `_SESSION_TOKEN`, else the standard `AWS_*` variables.
    pub fn upload_from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        for prefix in ["CARLA_PACK_UPLOAD_", "AWS_"] {
            if let (Some(id), Some(secret)) =
                (var(&format!("{prefix}ACCESS_KEY_ID")), var(&format!("{prefix}SECRET_ACCESS_KEY")))
            {
                return Some(Self {
                    access_key_id: id,
                    secret_access_key: secret,
                    session_token: var(&format!("{prefix}SESSION_TOKEN")),
                });
            }
        }
        None
    }
}

/// One request to sign. `canonical_uri` must already be URI-encoded (each path segment,
/// `/` kept) and `canonical_query` already sorted and encoded (empty for none).
pub struct Request<'a> {
    pub method: &'a str,
    /// `host` or `host:port` exactly as sent in the Host header.
    pub host: &'a str,
    pub canonical_uri: &'a str,
    pub canonical_query: &'a str,
    /// Extra headers to sign (lower-case names), e.g. `range`.
    pub headers: &'a [(&'a str, &'a str)],
    pub payload_hash: &'a str,
}

/// Returns the headers to add to the request: `x-amz-date`, `x-amz-content-sha256`,
/// `x-amz-security-token` (session credentials only) and `authorization`.
pub fn sign(req: &Request<'_>, creds: &Credentials, region: &str, amz_date: &str) -> Vec<(&'static str, String)> {
    let date = &amz_date[..8];
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), req.host.to_string()),
        ("x-amz-content-sha256".into(), req.payload_hash.to_string()),
        ("x-amz-date".into(), amz_date.to_string()),
    ];
    if let Some(token) = &creds.session_token {
        headers.push(("x-amz-security-token".into(), token.clone()));
    }
    headers.extend(req.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string())));
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical_request = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}",
        req.method, req.canonical_uri, req.canonical_query, req.payload_hash
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex(&Sha256::digest(canonical_request)));
    let mut key = hmac(format!("AWS4{}", creds.secret_access_key).as_bytes(), date.as_bytes());
    for part in [region, "s3", "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));

    let mut out = vec![("x-amz-date", amz_date.to_string()), ("x-amz-content-sha256", req.payload_hash.to_string())];
    if let Some(token) = &creds.session_token {
        out.push(("x-amz-security-token", token.clone()));
    }
    out.push((
        "authorization",
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed_headers},Signature={signature}",
            creds.access_key_id
        ),
    ));
    out
}

/// URI-encodes an object key for the canonical URI: unreserved characters and `/` kept.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Example: GET Object" from the AWS S3 SigV4 documentation (header-based authentication).
    #[test]
    fn matches_the_aws_get_object_example() {
        let creds = Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let req = Request {
            method: "GET",
            host: "examplebucket.s3.amazonaws.com",
            canonical_uri: "/test.txt",
            canonical_query: "",
            headers: &[("range", "bytes=0-9")],
            payload_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        };
        let headers = sign(&req, &creds, "us-east-1", "20130524T000000Z");
        let auth = &headers.iter().find(|(k, _)| *k == "authorization").unwrap().1;
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn session_tokens_are_signed_and_sent() {
        let creds =
            Credentials { access_key_id: "A".into(), secret_access_key: "S".into(), session_token: Some("T".into()) };
        let req = Request {
            method: "GET",
            host: "h",
            canonical_uri: "/",
            canonical_query: "",
            headers: &[],
            payload_hash: UNSIGNED_PAYLOAD,
        };
        let headers = sign(&req, &creds, "auto", "20260101T000000Z");
        assert!(headers.contains(&("x-amz-security-token", "T".into())));
        assert!(
            headers
                .last()
                .unwrap()
                .1
                .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token")
        );
        let debug = format!("{creds:?}");
        assert!(!debug.contains("\"S\"") && !debug.contains("\"T\""), "{debug}");
    }

    #[test]
    fn encodes_keys() {
        assert_eq!(encode_path("/b/packs/A B+c~d.tar.gz"), "/b/packs/A%20B%2Bc~d.tar.gz");
        assert_eq!(encode_path("/b/日"), "/b/%E6%97%A5");
    }
}
