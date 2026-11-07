//! Installing a pack tarball: archive safety, manifest checks, idempotency and --force.
mod common;

use std::fs;
use std::io::Write;
use std::path::Path;

use carla_pack_registry::Error;
use carla_pack_registry::pack::{self, Expected, InstallOptions, InstalledPack};
use common::*;

const EXPECTED: Expected<'static> =
    Expected { name: "MiniPack", version: "1.0.0", base_release: "carla-0.10.0-Linux", platform: "Linux" };

fn install_as(
    body: &[u8],
    packs_dir: &Path,
    expected: &Expected<'_>,
    opts: InstallOptions,
) -> carla_pack_registry::Result<InstalledPack> {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("pack.tar.gz");
    fs::write(&tarball, body).unwrap();
    pack::install(&tarball, packs_dir, expected, &opts)
}

fn install(body: &[u8], packs_dir: &Path, opts: InstallOptions) -> carla_pack_registry::Result<InstalledPack> {
    install_as(body, packs_dir, &EXPECTED, opts)
}

#[test]
fn installs_a_verified_pack() {
    let dir = tempfile::tempdir().unwrap();
    let packs = dir.path().join("Packs");
    let body = mini_pack(&[]);
    let pack = install(&body, &packs, InstallOptions::default()).unwrap();

    assert_eq!((pack.name.as_str(), pack.version.as_str()), ("MiniPack", "1.0.0"));
    assert_eq!((pack.base_release.as_str(), pack.platform.as_str()), ("carla-0.10.0-Linux", "Linux"));
    assert_eq!(pack.maps, ["MiniTown"]);
    assert_eq!((pack.path.as_path(), pack.bytes), (packs.join("MiniPack").as_path(), body.len() as u64));
    assert!(!pack.already_installed);
    assert_eq!(fs::read(packs.join("MiniPack/Content/Paks/Linux/MiniPack-Linux.pak")).unwrap(), PAK);
    assert_eq!(entries(&packs), ["MiniPack"]); // no scratch folder left behind

    let again = install(&body, &packs, InstallOptions::default()).unwrap();
    assert!(again.already_installed);
}

#[test]
fn reads_the_manifest_of_a_tarball() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("p.tar.gz");
    fs::write(&tarball, mini_pack_version("2.1.0", &[])).unwrap();
    let m = pack::read_manifest(&tarball).unwrap();
    assert_eq!(
        (m.name.as_str(), m.version.as_str(), m.map_names()),
        ("MiniPack", "2.1.0", vec!["MiniTown".to_string()])
    );
    fs::write(&tarball, targz(&[("MiniPack/a.pak", Member::File(PAK))])).unwrap();
    assert!(matches!(pack::read_manifest(&tarball), Err(Error::Pack(_))));
}

#[test]
fn refuses_tampered_unlisted_and_unsafe_archives() {
    let m = manifest("MiniPack", "1.0.0", &[("a.pak", PAK)]);
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
        match install(&body, &packs, InstallOptions::default()) {
            Err(Error::Pack(msg)) => assert!(msg.contains(expected), "{what}: {msg}"),
            other => panic!("{what}: unexpected {other:?}"),
        }
        assert!(entries(&packs).is_empty(), "{what}: left {:?}", entries(&packs));
        assert!(!dir.path().join("escaped").exists(), "{what}");
    }
}

#[test]
fn refuses_a_pack_that_is_not_what_the_repository_manifest_says() {
    let dir = tempfile::tempdir().unwrap();
    let lie = Expected { version: "9.9.9", ..EXPECTED };
    match install_as(&mini_pack(&[]), dir.path(), &lie, InstallOptions::default()) {
        Err(Error::Pack(msg)) => assert!(msg.contains("9.9.9") && msg.contains("1.0.0"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(entries(dir.path()).is_empty());
}

#[test]
fn refuses_another_base_release_before_installing() {
    let dir = tempfile::tempdir().unwrap();
    let opts = InstallOptions { base_release: Some("carla-0.10.0-Win64".into()), force: false };
    match install(&mini_pack(&[]), dir.path(), opts) {
        Err(e @ Error::Incompatible(_)) => {
            assert_eq!(e.code(), "incompatible_pack");
            assert!(e.to_string().contains("carla-0.10.0-Linux"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(entries(dir.path()).is_empty());
}

#[test]
fn replaces_another_version_only_with_force() {
    let dir = tempfile::tempdir().unwrap();
    install(&mini_pack(&[]), dir.path(), InstallOptions::default()).unwrap();
    let stale = dir.path().join("MiniPack/stale.txt");
    fs::File::create(&stale).unwrap().write_all(b"old").unwrap();

    let v2 = Expected { version: "2.0.0", ..EXPECTED };
    match install_as(&mini_pack_version("2.0.0", &[]), dir.path(), &v2, InstallOptions::default()) {
        Err(Error::Usage(msg)) => assert!(msg.contains("another version"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(stale.exists()); // untouched

    let opts = InstallOptions { base_release: None, force: true };
    let pack = install_as(&mini_pack_version("2.0.0", &[]), dir.path(), &v2, opts).unwrap();
    assert_eq!(pack.version, "2.0.0");
    assert!(!stale.exists());
    assert_eq!(entries(dir.path()), ["MiniPack"]);
}
