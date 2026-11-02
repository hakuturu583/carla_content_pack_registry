//! The client: find a pack in a GitHub repository the user can read, download it from the
//! repository's S3-compatible bucket, verify it and install it.
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client as Http;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::github::{GitHub, Vended};
use crate::manifest::{
    CredentialMode, Filter, HfStorage, Manifest, PackEntry, PackRef, Storage, StorageConfig, is_sha256, select,
    validate_repo,
};
use crate::pack::{self, Expected, InstallOptions, InstalledPack};
use crate::s3::Bucket;
use crate::sigv4::{Credentials, hex};
use crate::store::{self, Store, Token};
use crate::time;

/// How often a broken download is resumed (HTTP Range) before giving up.
const DOWNLOAD_RESUMES: u32 = 5;
const RESUME_DELAY: Duration = Duration::from_secs(2);

pub(crate) fn http_client() -> Result<Http> {
    Ok(Http::builder()
        .user_agent(concat!("carla-pack/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .build()?)
}

/// One pack release in one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub repo: String,
    #[serde(flatten)]
    pub pack: PackEntry,
}

impl Release {
    /// `OWNER/REPO/PACK@VERSION`.
    pub fn reference(&self) -> String {
        format!("{}/{}@{}", self.repo, self.pack.name, self.pack.version)
    }

    fn expected(&self) -> Expected<'_> {
        Expected {
            name: &self.pack.name,
            version: &self.pack.version,
            base_release: &self.pack.base_release,
            platform: &self.pack.platform,
        }
    }
}

/// What `credentials_status` reports (never the secrets themselves).
#[derive(Debug, Clone, Serialize)]
pub struct CredentialStatus {
    pub repo: String,
    /// "bucket NAME" or "Hugging Face dataset OWNER/NAME".
    pub storage: String,
    pub mode: CredentialMode,
    pub access_key_id: Option<String>,
    pub expires_at: Option<String>,
}

pub struct Client {
    github: GitHub,
    http: Http,
    token_source: Option<&'static str>,
    cache_dir: PathBuf,
    repos: Vec<String>,
    manifests: Mutex<HashMap<String, Manifest>>,
    credentials: Mutex<HashMap<String, Vended>>,
}

/// Everything a [`Client`] reads from the environment, for embedding and tests.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub token: Option<Token>,
    pub repos: Vec<String>,
    pub github_api: String,
    pub cache_dir: PathBuf,
}

impl Client {
    /// `token`: explicit GitHub token, else the lookup in [`store::find_token`].
    /// `repos`: repositories searched by map name, else `CARLA_PACK_REPOS` / the saved list.
    pub fn new(token: Option<String>, repos: Option<Vec<String>>) -> Result<Self> {
        let saved = Store::load()?;
        let token = match token {
            Some(value) => Some(Token { value, source: "argument" }),
            None => store::find_token(&saved),
        };
        Self::with_options(ClientOptions {
            token,
            repos: repos.unwrap_or_else(|| saved.search_repos()),
            github_api: GitHub::default_api(),
            cache_dir: store::cache_dir()?,
        })
    }

    pub fn with_options(opts: ClientOptions) -> Result<Self> {
        for r in &opts.repos {
            validate_repo(r)?;
        }
        let http = http_client()?;
        Ok(Self {
            github: GitHub::new(&opts.github_api, opts.token.as_ref().map(|t| t.value.clone()), http.clone()),
            http,
            token_source: opts.token.map(|t| t.source),
            cache_dir: opts.cache_dir,
            repos: opts.repos,
            manifests: Mutex::default(),
            credentials: Mutex::default(),
        })
    }

    /// Where the token came from (`GH_TOKEN`, `carla-pack login`, `gh auth token`, ...), `None` when anonymous.
    pub fn token_source(&self) -> Option<&'static str> {
        self.token_source
    }

    pub fn repos(&self) -> &[String] {
        &self.repos
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The GitHub user of the token.
    pub fn whoami(&self) -> Result<serde_json::Value> {
        self.github.user()
    }

    /// `carla-packs.toml` of `repo`, read once per client. Fails with `NoAccess` when the
    /// token cannot read the repository: this is the access check.
    pub fn manifest(&self, repo: &str) -> Result<Manifest> {
        validate_repo(repo)?;
        let key = repo.to_ascii_lowercase();
        if let Some(m) = self.manifests.lock().unwrap().get(&key) {
            return Ok(m.clone());
        }
        let m = self.github.manifest(repo).map_err(|e| match e {
            Error::NoAccess(what) if self.github.has_token() => Error::NoAccess(what),
            Error::NoAccess(what) => {
                Error::Unauthenticated(format!("{what} is not public and no GitHub token was found"))
            }
            e => e,
        })?;
        self.manifests.lock().unwrap().insert(key, m.clone());
        Ok(m)
    }

    /// Every release `repo` lists, newest first.
    pub fn list_packs(&self, repo: &str) -> Result<Vec<Release>> {
        let m = self.manifest(repo)?;
        let mut out: Vec<Release> = m.packs.into_iter().map(|pack| Release { repo: repo.to_string(), pack }).collect();
        out.sort_by(|a, b| {
            a.pack.name.cmp(&b.pack.name).then(crate::manifest::compare_versions(&b.pack.version, &a.pack.version))
        });
        Ok(out)
    }

    /// The release `reference` names, narrowed by `filter`: the newest version unless the
    /// reference has one.
    pub fn resolve(&self, reference: &PackRef, filter: &Filter) -> Result<Release> {
        let m = self.manifest(&reference.repo)?;
        let candidates: Vec<&PackEntry> = m
            .packs
            .iter()
            .filter(|p| p.name == reference.name && reference.version.as_ref().is_none_or(|v| *v == p.version))
            .filter(|p| filter.matches(p))
            .collect();
        let pack = select(candidates, |p| *p, &reference.to_string())?.clone();
        Ok(Release { repo: reference.repo.clone(), pack })
    }

    /// The newest release providing the CARLA map `map` in `repos` (default: the client's
    /// search list). Repositories the token cannot read are skipped; if nothing is found,
    /// the error says which ones were.
    pub fn find_map(&self, map: &str, repos: Option<&[String]>, filter: &Filter) -> Result<Release> {
        let repos = repos.unwrap_or(&self.repos);
        if repos.is_empty() {
            return Err(Error::Usage(
                "no pack repositories to search; run `carla-pack repo add OWNER/REPO` or set CARLA_PACK_REPOS".into(),
            ));
        }
        let short = |m: &str| m.rsplit('/').next().unwrap_or(m).to_string();
        let wanted = short(map);
        let mut candidates = Vec::new();
        let mut unreadable = Vec::new();
        for repo in repos {
            let m = match self.manifest(repo) {
                Ok(m) => m,
                Err(e @ (Error::NoAccess(_) | Error::Unauthenticated(_))) => {
                    unreadable.push(format!("{repo} ({})", e.code()));
                    continue;
                }
                Err(e) => return Err(e),
            };
            for p in m.packs {
                if filter.matches(&p) && p.maps.iter().any(|x| short(x) == wanted) {
                    candidates.push(Release { repo: repo.clone(), pack: p });
                }
            }
        }
        select(candidates, |r| &r.pack, &format!("map {wanted}")).map_err(|e| match e {
            Error::NotFound(msg) if !unreadable.is_empty() => {
                Error::NotFound(format!("{msg}; not readable: {}", unreadable.join(", ")))
            }
            e => e,
        })
    }

    /// Storage credentials of `repo`, read once per client (and again once they near expiry).
    fn storage_credentials(&self, repo: &str, manifest: &Manifest) -> Result<Option<Credentials>> {
        let Storage::S3(storage) = &manifest.storage else { return Ok(None) };
        if storage.credentials == CredentialMode::Anonymous {
            return Ok(None);
        }
        let key = repo.to_ascii_lowercase();
        if let Some(v) = self.credentials.lock().unwrap().get(&key) {
            if v.expires_at.is_none_or(|t| t > time::now() + 300) {
                return Ok(Some(v.credentials.clone()));
            }
        }
        let vended = self.github.credentials(repo, &storage.credentials_tag)?;
        let creds = vended.credentials.clone();
        self.credentials.lock().unwrap().insert(key, vended);
        Ok(Some(creds))
    }

    pub fn credentials_status(&self, repo: &str) -> Result<CredentialStatus> {
        let m = self.manifest(repo)?;
        let mode = m.storage.credential_mode();
        let mut status = CredentialStatus {
            repo: repo.to_string(),
            storage: m.storage.describe(),
            mode,
            access_key_id: None,
            expires_at: None,
        };
        if let Storage::S3(StorageConfig { credentials: CredentialMode::GithubRelease, credentials_tag, .. }) =
            &m.storage
        {
            let v = self.github.credentials(repo, credentials_tag)?;
            status.access_key_id = Some(v.credentials.access_key_id);
            status.expires_at = v.expires_at.map(time::rfc3339);
        }
        Ok(status)
    }

    /// Path of the cached tarball for `sha256` (it may not exist).
    pub fn cached_path(&self, sha256: &str) -> PathBuf {
        self.cache_dir.join("blobs").join(format!("{sha256}.tar.gz"))
    }

    /// Downloads `release` into the cache (unless it is there already) and returns its path.
    /// The SHA-256 and size are checked before the file is moved into place.
    pub fn fetch(&self, release: &Release) -> Result<PathBuf> {
        let dest = self.cached_path(&release.pack.sha256);
        if fs::metadata(&dest).is_ok_and(|m| m.is_file() && m.len() == release.pack.size) {
            return Ok(dest);
        }
        let manifest = self.manifest(&release.repo)?;
        let creds = self.storage_credentials(&release.repo, &manifest)?;
        let source = Source::new(&manifest.storage, creds.as_ref());
        let object_key = manifest.storage.object_key(&release.pack.key);
        let mut res = source.get(&self.http, &object_key)?;

        let dir = dest.parent().expect("blob path has a parent");
        fs::create_dir_all(dir)?;
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let part = dir.join(format!(".{}.{}-{nanos}.part", release.pack.sha256, std::process::id()));
        let result = (|| -> Result<()> {
            let mut file = BufWriter::with_capacity(1 << 20, fs::File::create(&part)?);
            let mut hasher = Sha256::new();
            let mut buf = vec![0; 1 << 20];
            let mut total = 0u64;
            let mut resumes = 0;
            loop {
                let error = match res.read(&mut buf) {
                    Ok(0) if total >= release.pack.size => break,
                    Ok(0) => format!("the connection closed after {total} of {} bytes", release.pack.size),
                    Ok(n) => {
                        hasher.update(&buf[..n]);
                        file.write_all(&buf[..n])?;
                        total += n as u64;
                        if total > release.pack.size {
                            break; // reported by the size check below
                        }
                        continue;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => e.to_string(),
                };
                // The connection ended early: carry on from where it stopped. The hash covers
                // every byte, so a resumed download is checked like any other.
                if resumes >= DOWNLOAD_RESUMES {
                    return Err(Error::Usage(format!(
                        "downloading {object_key} failed after {resumes} resumed attempts: {error}"
                    )));
                }
                resumes += 1;
                std::thread::sleep(RESUME_DELAY * resumes);
                res = source.get_from(&self.http, &object_key, total)?.ok_or_else(|| {
                    Error::Usage(format!(
                        "downloading {object_key} broke off ({error}) and the storage cannot resume it"
                    ))
                })?;
            }
            file.flush()?;
            let digest = hex(&hasher.finalize());
            if total != release.pack.size || digest != release.pack.sha256 {
                return Err(Error::Integrity(format!(
                    "{object_key} from {} does not match its manifest entry (expected {} bytes, sha256 {}; got {total} \
                     bytes, sha256 {digest}); nothing was installed",
                    release.repo, release.pack.size, release.pack.sha256
                )));
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                fs::rename(&part, &dest)?;
                Ok(dest)
            }
            Err(e) => {
                let _ = fs::remove_file(&part);
                Err(e)
            }
        }
    }

    /// Fetches, verifies and installs `release` as `<packs_dir>/<name>`. An identical pack
    /// already installed is left alone without downloading anything.
    pub fn install(&self, release: &Release, packs_dir: &Path, opts: &InstallOptions) -> Result<InstalledPack> {
        if let Some(found) = pack::installed(packs_dir, &release.expected()) {
            if opts.base_release.as_deref().is_none_or(|b| b == release.pack.base_release) {
                return Ok(InstalledPack { bytes: release.pack.size, ..found });
            }
        }
        let tarball = self.fetch(release)?;
        pack::install(&tarball, packs_dir, &release.expected(), opts)
    }
}

/// Where a download comes from: a (signed) S3 bucket or a public Hugging Face repository.
enum Source<'a> {
    S3(Bucket<'a>),
    HuggingFace(&'a HfStorage),
}

impl<'a> Source<'a> {
    fn new(storage: &'a Storage, credentials: Option<&'a Credentials>) -> Self {
        match storage {
            Storage::S3(config) => Self::S3(Bucket { config, credentials }),
            Storage::HuggingFace(hf) => Self::HuggingFace(hf),
        }
    }

    fn get(&self, http: &Http, path: &str) -> Result<reqwest::blocking::Response> {
        match self {
            Self::S3(b) => b.get(http, path),
            Self::HuggingFace(hf) => crate::hf::get(http, hf, path),
        }
    }

    fn get_from(&self, http: &Http, path: &str, offset: u64) -> Result<Option<reqwest::blocking::Response>> {
        match self {
            Self::S3(b) => b.get_from(http, path, offset),
            Self::HuggingFace(hf) => crate::hf::get_from(http, hf, path, offset),
        }
    }
}

/// What `publish` uploads and appends to the manifest.
#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    /// Object key relative to `storage.prefix`; default: the tarball's file name.
    pub key: Option<String>,
    /// Upload the tarball (false: only add the manifest entry; it was uploaded otherwise).
    pub upload: bool,
    /// Replace an object already in the bucket under that key.
    pub overwrite: bool,
    pub description: Option<String>,
    /// Multipart part size in bytes (default 64 MiB); files up to one part go up in one PUT.
    pub part_size: Option<u64>,
}

/// Adds the pack `tarball` to the manifest file at `manifest_path` (a checkout of the pack
/// repository) and, with `opts.upload`, uploads it with `upload_credentials` (write access to
/// the bucket, from the environment). The manifest entry is appended, so comments survive;
/// commit and push it to publish.
pub fn publish(
    manifest_path: &Path,
    tarball: &Path,
    opts: &PublishOptions,
    upload_credentials: Option<&Credentials>,
) -> Result<PackEntry> {
    let text = fs::read_to_string(manifest_path)
        .map_err(|e| Error::Usage(format!("cannot read {}: {e}", manifest_path.display())))?;
    let manifest = Manifest::parse(&text, &manifest_path.display().to_string())?;
    let pm = pack::read_manifest(tarball)?;
    let key = match &opts.key {
        Some(k) => k.clone(),
        None => tarball
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::Usage("the tarball has no usable file name; pass a key".into()))?
            .to_string(),
    };
    let entry = PackEntry {
        maps: pm.map_names(),
        name: pm.name,
        version: pm.version,
        base_release: pm.base_release,
        platform: pm.platform,
        key,
        sha256: pack::sha256_file(tarball)?,
        size: fs::metadata(tarball)?.len(),
        description: opts.description.clone(),
        yanked: false,
    };
    debug_assert!(is_sha256(&entry.sha256));
    if let Some(p) = manifest.packs.iter().find(|p| {
        p.name == entry.name
            && p.version == entry.version
            && p.base_release == entry.base_release
            && p.platform.eq_ignore_ascii_case(&entry.platform)
    }) {
        return Err(Error::Usage(format!(
            "{} {} ({}, {}) is already published (sha256 {}); published releases never change, bump the version",
            p.name, p.version, p.base_release, p.platform, p.sha256
        )));
    }
    // Validate the new entry with the same rules as the rest of the file.
    let mut check = manifest.clone();
    check.packs.push(entry.clone());
    Manifest::parse(&toml::to_string(&check).map_err(|e| Error::Usage(e.to_string()))?, "the new entry")?;

    if opts.upload {
        let storage = match &manifest.storage {
            Storage::S3(s) => s,
            Storage::HuggingFace(hf) => {
                return Err(Error::Usage(format!(
                    "carla-pack does not upload to Hugging Face; upload with \
                     `hf upload {} {} {} --repo-type {}` (to revision {}), then publish with --no-upload",
                    hf.repo,
                    tarball.display(),
                    manifest.storage.object_key(&entry.key),
                    hf.repo_type.as_str(),
                    hf.revision
                )));
            }
        };
        let creds = upload_credentials.ok_or_else(|| {
            Error::Usage(
                "uploading needs write credentials for the bucket: set CARLA_PACK_UPLOAD_ACCESS_KEY_ID and \
                 CARLA_PACK_UPLOAD_SECRET_ACCESS_KEY (or AWS_*), or upload yourself and pass --no-upload"
                    .into(),
            )
        })?;
        let http = http_client()?;
        let bucket = Bucket { config: storage, credentials: Some(creds) };
        let object_key = manifest.storage.object_key(&entry.key);
        if !opts.overwrite && bucket.head(&http, &object_key)?.is_some() {
            return Err(Error::Usage(format!(
                "{object_key} already exists in bucket {}; pass overwrite to replace it",
                storage.bucket
            )));
        }
        bucket.put_file(&http, &object_key, tarball, opts.part_size.unwrap_or(crate::s3::DEFAULT_PART_SIZE))?;
    }

    #[derive(Serialize)]
    struct Append<'a> {
        pack: [&'a PackEntry; 1],
    }
    let block = toml::to_string(&Append { pack: [&entry] }).map_err(|e| Error::Usage(e.to_string()))?;
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&block);
    let tmp = manifest_path.with_extension("toml.tmp");
    fs::write(&tmp, &out)?;
    fs::rename(&tmp, manifest_path)?;
    Ok(entry)
}
