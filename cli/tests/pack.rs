//! Content pack installation against mock registry/storage servers.
use std::fs;
use std::io::Write;
use std::path::Path;

use carla_pack_registry::pack::InstallOptions;
use carla_pack_registry::{Client, Error};
use flate2::Compression;
use flate2::write::GzEncoder;
use httpmock::prelude::*;
use serde_json::json;
use sha2::{Digest, Sha256};

const BUCKET_ID: &str = "5d93316f-ced0-4419-836f-813b9d450715";
const KEY: &str = "packs/carla-0.10.0-Linux/MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz";

#[derive(Clone, Copy)]
enum Member<'a> {
    Dir,
    File(&'a [u8]),
    Symlink(&'a str),
}

/// A .tar.gz with members written verbatim (names are not sanitised, unlike tar::Builder's
/// path helpers, so hostile archives can be built too).
fn targz(members: &[(&str, Member)]) -> Vec<u8> {
    let mut b = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (name, m) in members {
        let mut h = tar::Header::new_gnu();
        h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_mode(0o644);
        match m {
            Member::Dir => {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_size(0);
                h.set_cksum();
                b.append(&h, std::io::empty()).unwrap();
            }
            Member::File(data) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_size(data.len() as u64);
                h.set_cksum();
                b.append(&h, *data).unwrap();
            }
            Member::Symlink(target) => {
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_link_name(target).unwrap();
                h.set_size(0);
                h.set_cksum();
                b.append(&h, std::io::empty()).unwrap();
            }
        }
    }
    b.into_inner().unwrap().finish().unwrap()
}

fn sha256(data: &[u8]) -> String {
    format!("sha256:{}", Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect::<String>())
}

const PAK: &[u8] = b"pak-bytes";
const XODR: &[u8] = b"<OpenDRIVE/>";

fn manifest(name: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let files: serde_json::Map<_, _> = files.iter().map(|(k, v)| (k.to_string(), json!(sha256(v)))).collect();
    serde_json::to_vec(&json!({
        "name": name, "version": "1.0.0", "carla_version": "0.10.0", "base_release": "carla-0.10.0-Linux",
        "engine": { "version": "5.8.0", "commit": "1f1548682db8" }, "platform": "Linux",
        "maps": [{ "name": "MiniTown", "package": "/MiniPack/Maps/MiniTown" }], "catalogs": [], "files": files,
    }))
    .unwrap()
}

/// A well-formed pack; `extra` members are appended as-is.
fn mini_pack(extra: &[(&str, Member)]) -> Vec<u8> {
    let m = manifest(
        "MiniPack",
        &[("Content/Paks/Linux/MiniPack-Linux.pak", PAK), ("Content/Maps/OpenDrive/MiniTown.xodr", XODR)],
    );
    let mut members = vec![
        ("MiniPack/", Member::Dir),
        ("MiniPack/carla-pack.json", Member::File(&m)),
        ("MiniPack/Content/Paks/Linux/MiniPack-Linux.pak", Member::File(PAK)),
        ("MiniPack/Content/Maps/OpenDrive/MiniTown.xodr", Member::File(XODR)),
    ];
    members.extend(extra.iter().copied());
    targz(&members)
}

/// Serves `body` as the object KEY and installs it into `packs_dir`.
fn install(
    body: Vec<u8>,
    packs_dir: &Path,
    opts: InstallOptions,
) -> carla_pack_registry::Result<carla_pack_registry::pack::InstalledPack> {
    let registry = MockServer::start();
    let storage = MockServer::start();
    registry.mock(|when, then| {
        when.method(GET).path(format!("/v1/buckets/{BUCKET_ID}/objects/{KEY}/download"));
        then.status(200).json_body(json!({ "url": storage.url("/obj"), "method": "GET", "expiresAt": "" }));
    });
    storage.mock(|when, then| {
        when.method(GET).path("/obj");
        then.status(200).body(body);
    });
    Client::new(&registry.base_url(), Some("t".into())).unwrap().install_pack(BUCKET_ID, KEY, packs_dir, &opts)
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<_> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn installs_a_verified_pack() {
    let dir = tempfile::tempdir().unwrap();
    let packs = dir.path().join("Packs");
    let body = mini_pack(&[]);
    let size = body.len() as u64;
    let pack = install(body, &packs, InstallOptions::default()).unwrap();

    assert_eq!((pack.name.as_str(), pack.version.as_str()), ("MiniPack", "1.0.0"));
    assert_eq!((pack.base_release.as_str(), pack.platform.as_str()), ("carla-0.10.0-Linux", "Linux"));
    assert_eq!(pack.maps, ["MiniTown"]);
    assert_eq!((pack.path.as_path(), pack.bytes), (packs.join("MiniPack").as_path(), size));
    assert_eq!(fs::read(packs.join("MiniPack/Content/Paks/Linux/MiniPack-Linux.pak")).unwrap(), PAK);
    assert!(packs.join("MiniPack/carla-pack.json").is_file());
    assert_eq!(entries(&packs), ["MiniPack"]); // no download or scratch folder left behind
}

#[test]
fn refuses_tampered_unlisted_and_unsafe_archives() {
    let m = manifest("MiniPack", &[("a.pak", PAK)]);
    let tampered = targz(&[("MiniPack/carla-pack.json", Member::File(&m)), ("MiniPack/a.pak", Member::File(b"evil"))]);
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("tampered", tampered, "checksum mismatch for a.pak"),
        ("unlisted", mini_pack(&[("MiniPack/Content/extra.bin", Member::File(b"x"))]), "not listed in the manifest"),
        ("parent", mini_pack(&[("MiniPack/../../escaped", Member::File(b"x"))]), "refusing archive member"),
        ("absolute", mini_pack(&[("/tmp/absolute-escape", Member::File(b"x"))]), "refusing archive member"),
        ("symlink", mini_pack(&[("MiniPack/link", Member::Symlink("/etc"))]), "non-regular archive member"),
        ("two tops", mini_pack(&[("Other/x", Member::File(b"x"))]), "single top-level pack folder"),
        ("no manifest", targz(&[("MiniPack/a.pak", Member::File(PAK))]), "no carla-pack.json"),
        ("not gzip", b"plain text".to_vec(), "as a .tar.gz pack"),
    ];
    for (what, body, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let packs = dir.path().join("Packs");
        match install(body, &packs, InstallOptions::default()) {
            Err(Error::Pack(msg)) => assert!(msg.contains(expected), "{what}: {msg}"),
            other => panic!("{what}: unexpected {other:?}"),
        }
        assert!(entries(&packs).is_empty(), "{what}: left {:?}", entries(&packs));
        assert!(!dir.path().join("escaped").exists(), "{what}");
    }
}

#[test]
fn refuses_another_base_release_before_installing() {
    let dir = tempfile::tempdir().unwrap();
    let opts = InstallOptions { base_release: Some("carla-0.10.0-Win64".into()), force: false };
    match install(mini_pack(&[]), dir.path(), opts) {
        Err(e @ Error::Incompatible(_)) => {
            assert_eq!(e.code(), "incompatible_pack");
            assert!(e.to_string().contains("carla-0.10.0-Linux"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(entries(dir.path()).is_empty());
}

#[test]
fn replaces_an_installed_pack_only_with_force() {
    let dir = tempfile::tempdir().unwrap();
    install(mini_pack(&[]), dir.path(), InstallOptions::default()).unwrap();
    let stale = dir.path().join("MiniPack/stale.txt");
    fs::File::create(&stale).unwrap().write_all(b"old").unwrap();

    match install(mini_pack(&[]), dir.path(), InstallOptions::default()) {
        Err(Error::Usage(msg)) => assert!(msg.contains("already installed")),
        other => panic!("unexpected {other:?}"),
    }
    assert!(stale.exists()); // untouched

    install(mini_pack(&[]), dir.path(), InstallOptions { base_release: None, force: true }).unwrap();
    assert!(!stale.exists());
    assert_eq!(entries(dir.path()), ["MiniPack"]);
}
