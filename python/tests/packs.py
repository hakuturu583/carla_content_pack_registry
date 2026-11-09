"""Pack tarballs as CARLA's Util/ContentPacks/carla_pack.py builds them."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import random
import tarfile

FILES = {
    "Content/Paks/Linux/MiniPack-Linux.pak": b"pak-bytes",
    "Content/Maps/OpenDrive/MiniTown.xodr": b"<OpenDRIVE/>",
}


def mini_pack(version: str = "1.0.0", extra: dict[str, bytes] | None = None) -> bytes:
    """MiniPack <version>, providing the map MiniTown. Deterministic bytes per version."""
    files = {**FILES, **(extra or {})}
    manifest = {
        "name": "MiniPack",
        "version": version,
        "base_release": "carla-0.10.0-Linux",
        "platform": "Linux",
        "maps": [{"name": "MiniTown", "package": "/MiniPack/Maps/MiniTown"}],
        "files": {k: "sha256:" + hashlib.sha256(v).hexdigest() for k, v in files.items()},
    }
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as gz, tarfile.open(fileobj=gz, mode="w") as tar:
        for name, data in [("carla-pack.json", json.dumps(manifest).encode()), *files.items()]:
            info = tarfile.TarInfo(f"MiniPack/{name}")
            info.size = len(data)
            info.mtime = 0
            tar.addfile(info, io.BytesIO(data))
    return out.getvalue()


def big_pack(version: str, min_len: int) -> bytes:
    """A pack over `min_len` bytes: incompressible padding listed in its manifest."""
    padding = random.Random(version).randbytes(min_len)
    return mini_pack(version, {"Content/padding.bin": padding})


HD_FILES = {
    "Content/Paks/Linux/HDPack-Linux.pak": b"pak-bytes",
    "Content/Maps/OpenDrive/HDTown/HDTown.xodr": b"<OpenDRIVE/>",
    "Content/Maps/Nav/HDTown.bin": b"navmesh",
    "Content/HDMaps/HDTown/autoware/lanelet2_map.osm": b"<osm/>",
    "Content/HDMaps/HDTown/autoware/pointcloud_map.pcd": b"PCD",
    "Content/HDMaps/HDTown/autoware/map_projector_info.yaml": b"projector_type: Local\n",
    "Content/HDMaps/HDTown/sumo/HDTown.netccfg": b"<configuration/>",
    "Content/HDMaps/OnlyData/osm/OnlyData.osm": b"<osm/>",
}

HD_ASSET = {
    "schema": 1,
    "generator": {"name": "roadgen", "version": "0.43.0"},
    "maps": {
        "HDTown": {
            "projection": "local_cartesian",
            "formats": {
                "opendrive": {"type": "file", "path": "Content/Maps/OpenDrive/HDTown/HDTown.xodr"},
                "lanelet2": {"type": "file", "path": "Content/HDMaps/HDTown/autoware/lanelet2_map.osm"},
                "autoware": {
                    "type": "directory",
                    "path": "Content/HDMaps/HDTown/autoware",
                    "files": {
                        "lanelet2_map": "Content/HDMaps/HDTown/autoware/lanelet2_map.osm",
                        "pointcloud_map": "Content/HDMaps/HDTown/autoware/pointcloud_map.pcd",
                        "map_projector_info": "Content/HDMaps/HDTown/autoware/map_projector_info.yaml",
                    },
                },
                "sumo": {
                    "type": "directory",
                    "path": "Content/HDMaps/HDTown/sumo",
                    "files": {"netccfg": "Content/HDMaps/HDTown/sumo/HDTown.netccfg"},
                },
            },
        },
        # Data only: no CARLA level of this name.
        "OnlyData": {"formats": {"osm": {"type": "file", "path": "Content/HDMaps/OnlyData/osm/OnlyData.osm"}}},
    },
}


def hd_pack(version: str = "1.0.0", asset: dict | None = HD_ASSET) -> bytes:
    """HDPack <version>: the map HDTown, with its OpenDRIVE and navigation mesh, and (unless
    `asset` is None) a roadgen_asset.json naming it in other formats and a data-only map."""
    files = dict(HD_FILES)
    if asset is not None:
        files["Content/Config/roadgen_asset.json"] = json.dumps(asset).encode()
    manifest = {
        "name": "HDPack",
        "version": version,
        "base_release": "carla-0.10.0-Linux",
        "platform": "Linux",
        "maps": [
            {
                "name": "HDTown",
                "package": "/HDPack/Maps/HDTown",
                "xodr": "Maps/OpenDrive/HDTown/HDTown.xodr",
                "nav": "Maps/Nav/HDTown.bin",
            }
        ],
        "files": {k: "sha256:" + hashlib.sha256(v).hexdigest() for k, v in files.items()},
    }
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as gz, tarfile.open(fileobj=gz, mode="w") as tar:
        for name, data in [("carla-pack.json", json.dumps(manifest).encode()), *files.items()]:
            info = tarfile.TarInfo(f"HDPack/{name}")
            info.size = len(data)
            info.mtime = 0
            tar.addfile(info, io.BytesIO(data))
    return out.getvalue()
