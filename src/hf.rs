//! Downloads from a public Hugging Face repository: anonymous `resolve` URLs, which
//! redirect to the Hub's CDN. No token is sent, ever; the packs here are public and the
//! SHA-256 in the manifest is what makes the bytes trustworthy.
use std::time::Duration;

use reqwest::blocking::{Client as Http, Response};
use reqwest::{StatusCode, Url};

use crate::error::{Error, Result};
use crate::manifest::{HfRepoType, HfStorage};
use crate::sigv4::encode_path;

/// `https://huggingface.co/datasets/OWNER/NAME/resolve/REVISION/PATH`.
pub fn url(config: &HfStorage, path: &str) -> Result<Url> {
    let kind = match config.repo_type {
        HfRepoType::Dataset => "datasets/",
        HfRepoType::Model => "",
    };
    let text = format!(
        "{}/{kind}{}/resolve/{}/{}",
        config.endpoint.trim_end_matches('/'),
        config.repo,
        encode_path(&config.revision).replace('/', "%2F"),
        encode_path(path)
    );
    Url::parse(&text).map_err(|e| Error::Manifest(format!("cannot build a Hugging Face URL: {e}")))
}

pub fn get(http: &Http, config: &HfStorage, path: &str) -> Result<Response> {
    let res = http.get(url(config, path)?).timeout(Duration::from_secs(6 * 3600)).send()?;
    check(res, config, path)
}

/// GET from byte `offset` on; `None` when the Hub ignored the range.
pub fn get_from(http: &Http, config: &HfStorage, path: &str, offset: u64) -> Result<Option<Response>> {
    let res = http
        .get(url(config, path)?)
        .header("range", format!("bytes={offset}-"))
        .timeout(Duration::from_secs(6 * 3600))
        .send()?;
    let res = check(res, config, path)?;
    Ok((res.status() == StatusCode::PARTIAL_CONTENT).then_some(res))
}

fn check(res: Response, config: &HfStorage, path: &str) -> Result<Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let hint = match status.as_u16() {
        401 | 403 | 404 => {
            " (carla-pack reads Hugging Face anonymously: the repository must be public and not gated, \
             and the file must exist at that revision)"
        }
        _ => "",
    };
    Err(Error::Storage {
        status: status.as_u16(),
        message: format!(
            "{path} in Hugging Face {} {}@{}{hint}",
            config.repo_type.as_str(),
            config.repo,
            config.revision
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage(repo_type: HfRepoType, revision: &str) -> HfStorage {
        HfStorage {
            repo: "AutowareFoundation/carla-packs".into(),
            repo_type,
            revision: revision.into(),
            prefix: "packs/".into(),
            endpoint: "https://huggingface.co".into(),
        }
    }

    #[test]
    fn builds_resolve_urls() {
        let u = url(&storage(HfRepoType::Dataset, "main"), "packs/Town 01.tar.gz").unwrap();
        assert_eq!(
            u.as_str(),
            "https://huggingface.co/datasets/AutowareFoundation/carla-packs/resolve/main/packs/Town%2001.tar.gz"
        );
        let u = url(&storage(HfRepoType::Model, "refs/pr/1"), "a.tar.gz").unwrap();
        assert_eq!(u.as_str(), "https://huggingface.co/AutowareFoundation/carla-packs/resolve/refs%2Fpr%2F1/a.tar.gz");
    }
}
