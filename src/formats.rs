//! The map data a pack carries, by map and format.
//!
//! Every CARLA content pack carries a few files beside each level, which its
//! `carla-pack.json` names: the OpenDRIVE, the pedestrians' navigation mesh, the
//! Traffic Manager's data and the map logic. Those are the *standard* formats, and
//! every pack has them for every map it registers them for.
//!
//! A pack may also carry an asset file, `Content/Config/roadgen_asset.json`, naming
//! the same maps in other formats it carries — Lanelet2 and the point cloud map for
//! Autoware, SUMO, CommonRoad, ClipGT and the rest; roadgen writes one into a pack
//! with `roadgen.carla_pack_assets`. It is optional: a pack without one has its
//! standard formats only. Its paths, like these, are relative to the pack folder:
//!
//! ```json
//! {"schema": 1, "generator": {"name": "roadgen", "version": "0.43.0"},
//!  "maps": {"Highway": {"formats": {
//!     "lanelet2": {"type": "file", "path": "Content/HDMaps/Highway/autoware/lanelet2_map.osm"},
//!     "autoware": {"type": "directory", "path": "Content/HDMaps/Highway/autoware",
//!                  "files": {"lanelet2_map": "Content/HDMaps/Highway/autoware/lanelet2_map.osm"}}}}}}
//! ```
//!
//! In a built pack every file the asset file names must be one `carla-pack.json`
//! lists — that is what the install's SHA-256 check covers — or the asset file is
//! refused.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::pack::{MANIFEST_NAME, is_clean_relpath};

/// Where a pack carries its optional asset file, relative to the pack folder.
pub const ASSET_FILE: &str = "Content/Config/roadgen_asset.json";
/// The asset file schema this client reads.
pub const ASSET_SCHEMA: u32 = 1;

/// Whether a format is one file or a folder of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Directory,
}

/// Who says the pack carries a format: its `carla-pack.json`, or its asset file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Carla,
    Asset,
}

/// One map in one format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Format {
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Relative to the pack folder.
    pub path: String,
    /// A folder's files by role (`lanelet2_map`, `pointcloud_map`, …), relative to the
    /// pack folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    #[serde(default = "asset_origin")]
    pub origin: Origin,
}

fn asset_origin() -> Origin {
    Origin::Asset
}

/// Every map's formats, by map name and format name.
pub type MapFormats = BTreeMap<String, BTreeMap<String, Format>>;

/// The standard formats' names, and the `carla-pack.json` map keys they come from.
pub const STANDARD: [(&str, &str, Kind); 4] = [
    ("opendrive", "xodr", Kind::File),
    ("navmesh", "nav", Kind::File),
    ("traffic_manager", "tm", Kind::Directory),
    ("map_logic", "map_logic", Kind::File),
];

#[derive(Deserialize)]
struct CarlaManifest {
    #[serde(default)]
    maps: Vec<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct AssetFile {
    schema: u32,
    #[serde(default)]
    maps: BTreeMap<String, AssetMap>,
}

#[derive(Deserialize)]
struct AssetMap {
    #[serde(default)]
    formats: BTreeMap<String, Format>,
}

/// The formats a pack carries, from its `carla-pack.json` and, when there is one, its
/// asset file. A format both name is the standard one.
pub fn from_parts(manifest: &[u8], asset: Option<&[u8]>) -> Result<MapFormats> {
    let manifest: CarlaManifest =
        serde_json::from_slice(manifest).map_err(|e| Error::Pack(format!("invalid {MANIFEST_NAME}: {e}")))?;
    let mut out = MapFormats::new();
    for map in &manifest.maps {
        let Some(name) = map.get("name").and_then(|v| v.as_str()) else { continue };
        let formats = out.entry(name.to_string()).or_default();
        for (format, key, kind) in STANDARD {
            let Some(rel) = map.get(key).and_then(|v| v.as_str()).filter(|v| !v.is_empty()) else { continue };
            let path = format!("Content/{rel}");
            if !is_clean_relpath(&path) {
                return Err(Error::Pack(format!("{MANIFEST_NAME}: map {name} has a non-normalised {key} {rel:?}")));
            }
            let files = match kind {
                Kind::File => BTreeMap::new(),
                Kind::Directory => manifest
                    .files
                    .keys()
                    .filter_map(|f| f.strip_prefix(&format!("{path}/")).map(|role| (role.to_string(), f.clone())))
                    .collect(),
            };
            formats.insert(format.to_string(), Format { kind, path, files, origin: Origin::Carla });
        }
    }
    if let Some(raw) = asset {
        let asset: AssetFile =
            serde_json::from_slice(raw).map_err(|e| Error::Pack(format!("invalid {ASSET_FILE}: {e}")))?;
        if asset.schema != ASSET_SCHEMA {
            return Err(Error::Pack(format!(
                "{ASSET_FILE} has schema {}; this carla-pack reads schema {ASSET_SCHEMA}",
                asset.schema
            )));
        }
        let built = !manifest.files.is_empty();
        for (map, entry) in asset.maps {
            let formats = out.entry(map.clone()).or_default();
            for (name, mut format) in entry.formats {
                for rel in std::iter::once(&format.path).chain(format.files.values()) {
                    if !is_clean_relpath(rel) {
                        return Err(Error::Pack(format!(
                            "{ASSET_FILE}: {map} {name} has a non-normalised path {rel:?}"
                        )));
                    }
                }
                if built {
                    let carried = |rel: &str| {
                        manifest.files.contains_key(rel)
                            || manifest.files.keys().any(|f| f.starts_with(&format!("{rel}/")))
                    };
                    if let Some(missing) =
                        std::iter::once(&format.path).chain(format.files.values()).find(|rel| !carried(rel))
                    {
                        return Err(Error::Pack(format!(
                            "{ASSET_FILE}: {map} {name} names {missing}, which the pack's {MANIFEST_NAME} does not list"
                        )));
                    }
                }
                format.origin = Origin::Asset;
                formats.entry(name).or_insert(format);
            }
        }
    }
    Ok(out)
}

/// The formats of the pack folder `dir` (one holding `carla-pack.json`).
pub fn of_dir(dir: &Path) -> Result<MapFormats> {
    let manifest = fs::read(dir.join(MANIFEST_NAME))
        .map_err(|_| Error::Pack(format!("{} is not a content pack (no {MANIFEST_NAME})", dir.display())))?;
    let asset = fs::read(dir.join(ASSET_FILE)).ok();
    from_parts(&manifest, asset.as_deref())
}

/// The formats of the pack `tarball`, read out of it without unpacking the rest.
pub fn of_tarball(tarball: &Path) -> Result<MapFormats> {
    let mut archive = tar::Archive::new(GzDecoder::new(BufReader::new(fs::File::open(tarball)?)));
    let mut manifest = None;
    let mut asset = None;
    let bad = |e: std::io::Error| Error::Pack(format!("{} is not a readable pack archive: {e}", tarball.display()));
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        let parts: Vec<String> = path
            .components()
            .filter_map(|c| match c {
                Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let Some((_, rest)) = parts.split_first() else { continue };
        let rel = rest.join("/");
        if rel == MANIFEST_NAME || rel == ASSET_FILE {
            let mut raw = Vec::new();
            entry.read_to_end(&mut raw)?;
            if rel == MANIFEST_NAME {
                manifest = Some(raw);
            } else {
                asset = Some(raw);
            }
        }
    }
    let manifest =
        manifest.ok_or_else(|| Error::Pack(format!("{} holds no <Pack>/{MANIFEST_NAME}", tarball.display())))?;
    from_parts(&manifest, asset.as_deref())
}

/// Each map's format names: what a repository manifest indexes a release by.
pub fn index(formats: &MapFormats) -> BTreeMap<String, Vec<String>> {
    formats.iter().filter(|(_, f)| !f.is_empty()).map(|(map, f)| (map.clone(), f.keys().cloned().collect())).collect()
}

/// The path of `map` in `format` in the pack folder `dir`: the format's own path, or with
/// `file` one of a folder's files by role. The path is checked to exist.
pub fn resolve(dir: &Path, formats: &MapFormats, map: &str, format: &str, file: Option<&str>) -> Result<PathBuf> {
    let Some(of_map) = formats.get(map) else {
        let maps: BTreeSet<&str> = formats.keys().map(String::as_str).collect();
        return Err(Error::NotFound(format!(
            "the pack has no map {map:?} (maps: {})",
            maps.into_iter().collect::<Vec<_>>().join(", ")
        )));
    };
    let Some(entry) = of_map.get(format) else {
        return Err(Error::NotFound(format!(
            "map {map} has no {format:?} data (formats: {})",
            of_map.keys().cloned().collect::<Vec<_>>().join(", ")
        )));
    };
    let rel = match file {
        None => &entry.path,
        Some(role) => entry.files.get(role).ok_or_else(|| {
            Error::NotFound(format!(
                "map {map} {format} has no file {role:?} (files: {})",
                entry.files.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })?,
    };
    let path = dir.join(rel);
    let exists = match (entry.kind, file) {
        (Kind::Directory, None) => path.is_dir(),
        _ => path.is_file(),
    };
    if !exists {
        return Err(Error::Pack(format!("{rel} is named for map {map} {format} but is not in {}", dir.display())));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(files: &[&str]) -> Vec<u8> {
        let files: BTreeMap<&str, &str> = files.iter().map(|f| (*f, "0".repeat(64).leak() as &str)).collect();
        serde_json::to_vec(&serde_json::json!({
            "name": "P", "version": "1", "base_release": "b", "platform": "Linux",
            "maps": [{"name": "Town", "package": "/Game/Town", "xodr": "Maps/OpenDrive/Town/Town.xodr",
                      "nav": "Maps/Nav/Town.bin", "tm": "Maps/TM/Town"}],
            "files": files,
        }))
        .unwrap()
    }

    const ASSET: &str = r#"{"schema": 1, "maps": {"Town": {"formats": {
        "opendrive": {"type": "file", "path": "Content/HDMaps/Town/opendrive/Town.xodr"},
        "lanelet2": {"type": "file", "path": "Content/HDMaps/Town/autoware/lanelet2_map.osm"},
        "autoware": {"type": "directory", "path": "Content/HDMaps/Town/autoware",
                     "files": {"lanelet2_map": "Content/HDMaps/Town/autoware/lanelet2_map.osm"}}}}}}"#;

    #[test]
    fn a_plain_pack_has_its_standard_formats() {
        let m = manifest(&["Content/Maps/OpenDrive/Town/Town.xodr", "Content/Maps/TM/Town/Town.bin"]);
        let f = from_parts(&m, None).unwrap();
        let town = &f["Town"];
        assert_eq!(town.keys().collect::<Vec<_>>(), ["navmesh", "opendrive", "traffic_manager"]);
        assert_eq!(town["opendrive"].path, "Content/Maps/OpenDrive/Town/Town.xodr");
        assert_eq!(town["opendrive"].origin, Origin::Carla);
        assert_eq!(town["traffic_manager"].files["Town.bin"], "Content/Maps/TM/Town/Town.bin");
        assert_eq!(index(&f)["Town"], ["navmesh", "opendrive", "traffic_manager"]);
    }

    #[test]
    fn an_asset_file_adds_its_formats_and_the_standard_ones_win() {
        let m = manifest(&[
            "Content/Maps/OpenDrive/Town/Town.xodr",
            "Content/HDMaps/Town/opendrive/Town.xodr",
            "Content/HDMaps/Town/autoware/lanelet2_map.osm",
        ]);
        let f = from_parts(&m, Some(ASSET.as_bytes())).unwrap();
        let town = &f["Town"];
        assert_eq!(town["opendrive"].origin, Origin::Carla);
        assert_eq!(town["lanelet2"].origin, Origin::Asset);
        assert_eq!(town["autoware"].kind, Kind::Directory);
    }

    #[test]
    fn a_built_pack_must_carry_what_its_asset_file_names() {
        let m = manifest(&["Content/Maps/OpenDrive/Town/Town.xodr"]);
        let e = from_parts(&m, Some(ASSET.as_bytes())).unwrap_err();
        assert!(e.to_string().contains("does not list"), "{e}");
        // An unbuilt source folder (no files yet) is not checked.
        assert!(from_parts(&manifest(&[]), Some(ASSET.as_bytes())).is_ok());
    }

    #[test]
    fn an_asset_file_of_another_schema_or_escaping_the_pack_is_refused() {
        let m = manifest(&[]);
        assert!(from_parts(&m, Some(br#"{"schema": 2, "maps": {}}"#)).is_err());
        let escaping = r#"{"schema": 1, "maps": {"Town": {"formats": {
            "osm": {"type": "file", "path": "../../etc/passwd"}}}}}"#;
        assert!(from_parts(&m, Some(escaping.as_bytes())).is_err());
    }

    #[test]
    fn resolve_finds_a_format_and_a_folders_file() {
        let dir = std::env::temp_dir().join(format!("formats-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Content/HDMaps/Town/autoware")).unwrap();
        fs::write(dir.join("Content/HDMaps/Town/autoware/lanelet2_map.osm"), "<osm/>").unwrap();
        let f = from_parts(&manifest(&[]), Some(ASSET.as_bytes())).unwrap();
        assert!(resolve(&dir, &f, "Town", "autoware", None).unwrap().is_dir());
        assert!(resolve(&dir, &f, "Town", "autoware", Some("lanelet2_map")).unwrap().is_file());
        assert!(resolve(&dir, &f, "Town", "lanelet2", None).is_ok());
        assert!(matches!(resolve(&dir, &f, "Town", "sumo", None), Err(Error::NotFound(_))));
        assert!(matches!(resolve(&dir, &f, "Nowhere", "lanelet2", None), Err(Error::NotFound(_))));
        // Named but not there.
        assert!(matches!(resolve(&dir, &f, "Town", "opendrive", None), Err(Error::Pack(_))));
        let _ = fs::remove_dir_all(&dir);
    }
}
