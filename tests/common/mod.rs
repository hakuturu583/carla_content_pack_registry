//! Pack tarballs for tests: well-formed ones and hostile ones.
#![allow(dead_code)]
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub enum Member<'a> {
    Dir,
    File(&'a [u8]),
    Symlink(&'a str),
}

/// A .tar.gz with members written verbatim (names are not sanitised, unlike tar::Builder's
/// path helpers, so hostile archives can be built too).
pub fn targz(members: &[(&str, Member)]) -> Vec<u8> {
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

pub fn hex_sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

pub const PAK: &[u8] = b"pak-bytes";
pub const XODR: &[u8] = b"<OpenDRIVE/>";

pub fn manifest(name: &str, version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let files: serde_json::Map<_, _> =
        files.iter().map(|(k, v)| (k.to_string(), json!(format!("sha256:{}", hex_sha256(v))))).collect();
    serde_json::to_vec(&json!({
        "name": name, "version": version, "carla_version": "0.10.0", "base_release": "carla-0.10.0-Linux",
        "engine": { "version": "5.8.0", "commit": "1f1548682db8" }, "platform": "Linux",
        "maps": [{ "name": "MiniTown", "package": "/MiniPack/Maps/MiniTown" }], "catalogs": [], "files": files,
    }))
    .unwrap()
}

/// A well-formed MiniPack providing the map MiniTown; `extra` members are appended as-is.
pub fn mini_pack_version(version: &str, extra: &[(&str, Member)]) -> Vec<u8> {
    let m = manifest(
        "MiniPack",
        version,
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

pub fn mini_pack(extra: &[(&str, Member)]) -> Vec<u8> {
    mini_pack_version("1.0.0", extra)
}

pub fn entries(dir: &std::path::Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<_> = rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}
