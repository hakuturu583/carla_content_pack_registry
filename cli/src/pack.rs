//! Content pack installation: a pack tarball (built by CARLA's `Util/ContentPacks/carla_pack.py`)
//! is downloaded from a bucket, checked against its own `carla-pack.json` and moved into a
//! packs directory a CARLA server mounts from (`<package>/Linux/CarlaUnreal/Packs`,
//! `-carla-packs=<dir>` or `carla.Client.mount_content_pack`).
use std::collections::HashSet;
use std::fs;
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::client::Client;
use crate::error::{Error, Result};

/// The manifest at the top of every pack; the only file its own `files` map does not list.
pub const MANIFEST_NAME: &str = "carla-pack.json";

/// The part of `carla-pack.json` the installer checks.
#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    version: String,
    base_release: String,
    platform: String,
    #[serde(default)]
    maps: Vec<MapEntry>,
    files: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct MapEntry {
    name: String,
}

/// An installed pack, as returned to the CLI and Python.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledPack {
    pub name: String,
    pub version: String,
    pub base_release: String,
    pub platform: String,
    pub maps: Vec<String>,
    /// `<packs_dir>/<name>`: the folder to pass to `mount_content_pack`.
    pub path: PathBuf,
    /// Size of the downloaded tarball.
    pub bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Refuse a pack built against another base release (the server's `BaseRelease` stamp).
    pub base_release: Option<String>,
    /// Replace a pack of the same name already in `packs_dir`.
    pub force: bool,
}

impl Client {
    /// Downloads the pack tarball `key` of bucket `id`, verifies it and installs it as
    /// `<packs_dir>/<name>`. Nothing is left in `packs_dir` when any step fails.
    pub fn install_pack(&self, id: &str, key: &str, packs_dir: &Path, opts: &InstallOptions) -> Result<InstalledPack> {
        fs::create_dir_all(packs_dir)?;
        let work = WorkDir::create(packs_dir)?;
        let tarball = work.0.join("pack.tar.gz");
        let bytes = self.download(id, key, &tarball)?;
        let mut pack = install_tarball(&tarball, &work.0.join("x"), packs_dir, opts)?;
        pack.bytes = bytes;
        Ok(pack)
    }
}

/// Extracts and verifies `tarball` under `extract_to`, then moves the pack into `packs_dir`.
fn install_tarball(
    tarball: &Path,
    extract_to: &Path,
    packs_dir: &Path,
    opts: &InstallOptions,
) -> Result<InstalledPack> {
    let top = extract(tarball, extract_to)?;
    let manifest = verify(&top)?;
    if top.file_name().and_then(|n| n.to_str()) != Some(manifest.name.as_str()) {
        return Err(Error::Pack(format!(
            "archive folder {:?} does not match pack name {:?}",
            top.display(),
            manifest.name
        )));
    }
    if let Some(expected) = opts.base_release.as_deref().filter(|e| *e != manifest.base_release) {
        return Err(Error::Incompatible(format!(
            "pack {} was built against base release {:?}, not {expected:?}",
            manifest.name, manifest.base_release
        )));
    }
    let target = packs_dir.join(&manifest.name);
    if fs::symlink_metadata(&target).is_ok() {
        if !opts.force {
            return Err(Error::Usage(format!(
                "{} is already installed at {} (force replaces it; unmount it or restart the server if it is loaded)",
                manifest.name,
                target.display()
            )));
        }
        remove_any(&target)?;
    }
    fs::rename(&top, &target)?;
    Ok(InstalledPack {
        name: manifest.name,
        version: manifest.version,
        base_release: manifest.base_release,
        platform: manifest.platform,
        maps: manifest.maps.into_iter().map(|m| m.name).collect(),
        path: target,
        bytes: 0,
    })
}

/// Unpacks a `.tar.gz` holding one top-level folder into `dest` and returns that folder.
/// Only regular files and directories with plain relative paths are accepted.
fn extract(tarball: &Path, dest: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dest)?;
    let mut archive = tar::Archive::new(GzDecoder::new(BufReader::new(fs::File::open(tarball)?)));
    let mut top: Option<std::ffi::OsString> = None;
    for entry in archive.entries().map_err(|e| bad_archive(tarball, e))? {
        let mut entry = entry.map_err(|e| bad_archive(tarball, e))?;
        if entry.header().entry_type() == tar::EntryType::XGlobalHeader {
            continue; // pax global metadata, not a member
        }
        let name = entry.path().map_err(|e| bad_archive(tarball, e))?.into_owned();
        let mut parts = Vec::new();
        for c in name.components() {
            match c {
                Component::Normal(p) => parts.push(p.to_owned()),
                Component::CurDir => {}
                _ => return Err(Error::Pack(format!("refusing archive member {:?}", name.display()))),
            }
        }
        let Some(first) = parts.first() else { continue };
        match &top {
            None => top = Some(first.clone()),
            Some(t) if t != first => {
                return Err(Error::Pack("archive must hold a single top-level pack folder".into()));
            }
            Some(_) => {}
        }
        let out: PathBuf = std::iter::once(dest.as_os_str().to_owned()).chain(parts).collect();
        match entry.header().entry_type() {
            tar::EntryType::Directory => fs::create_dir_all(&out)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent)?;
                }
                // create_new: a member listed twice must not silently replace the first copy.
                let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&out)?;
                io::copy(&mut entry, &mut file)?;
            }
            other => {
                return Err(Error::Pack(format!(
                    "refusing non-regular archive member {:?} ({other:?})",
                    name.display()
                )));
            }
        }
    }
    let top = top.ok_or_else(|| Error::Pack(format!("{} is empty", tarball.display())))?;
    Ok(dest.join(top))
}

/// Checks every file against the manifest's SHA-256 and refuses files it does not list.
fn verify(dir: &Path) -> Result<Manifest> {
    let raw = fs::read(dir.join(MANIFEST_NAME))
        .map_err(|_| Error::Pack(format!("not a content pack: no {MANIFEST_NAME} at the top of the archive")))?;
    let manifest: Manifest =
        serde_json::from_slice(&raw).map_err(|e| Error::Pack(format!("invalid {MANIFEST_NAME}: {e}")))?;
    if !is_pack_name(&manifest.name) {
        return Err(Error::Pack(format!("invalid pack name {:?}", manifest.name)));
    }
    if manifest.base_release.is_empty() || manifest.files.is_empty() {
        return Err(Error::Pack(format!("{} was never built (empty base_release or files)", manifest.name)));
    }
    for (rel, digest) in &manifest.files {
        if !is_clean_relpath(rel) {
            return Err(Error::Pack(format!("manifest lists a non-normalised path {rel:?}")));
        }
        let file = dir.join(rel);
        if !fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
            return Err(Error::Pack(format!("{rel} is listed in the manifest but missing")));
        }
        if sha256_file(&file)? != digest.trim_start_matches("sha256:").to_ascii_lowercase() {
            return Err(Error::Pack(format!("checksum mismatch for {rel}")));
        }
    }
    let listed: HashSet<&str> = manifest.files.keys().map(String::as_str).chain([MANIFEST_NAME]).collect();
    let mut extra = Vec::new();
    walk_files(dir, dir, &mut |rel| {
        if !listed.contains(rel.as_str()) {
            extra.push(rel);
        }
    })?;
    if !extra.is_empty() {
        return Err(Error::Pack(format!("files not listed in the manifest: {}", extra.join(", "))));
    }
    Ok(manifest)
}

fn walk_files(root: &Path, dir: &Path, f: &mut impl FnMut(String)) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk_files(root, &path, f)?;
        } else {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            f(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Same rule as CARLA's pack tool: the name is the Unreal mount point `/<Name>/`.
fn is_pack_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `a/b/c`: relative, no `.`/`..`/empty segments, no backslashes or drive prefixes.
fn is_clean_relpath(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.contains('\\')
        && !rel.split('/').next().unwrap_or("").contains(':')
        && rel.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

fn bad_archive(path: &Path, e: io::Error) -> Error {
    Error::Pack(format!("cannot read {} as a .tar.gz pack: {e}", path.display()))
}

fn remove_any(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)?
    } else {
        fs::remove_file(path)?
    }
    Ok(())
}

/// Scratch folder inside `packs_dir` (same filesystem, so the final move is a rename);
/// removed on drop whatever happened.
struct WorkDir(PathBuf);

impl WorkDir {
    fn create(packs_dir: &Path) -> Result<Self> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = packs_dir.join(format!(".carla-pack-install-{}-{nanos}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_names_and_paths() {
        assert!(is_pack_name("Town01Pack") && is_pack_name("A_1"));
        assert!(!is_pack_name("") && !is_pack_name("1Pack") && !is_pack_name("a-b") && !is_pack_name("../x"));
        assert!(is_clean_relpath("Content/Paks/Linux/x.pak"));
        for bad in ["", "/abs", "a/../b", "a//b", "./a", "a\\b", "C:/x"] {
            assert!(!is_clean_relpath(bad), "{bad}");
        }
    }
}
