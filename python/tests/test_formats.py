"""A pack's map data by format: the standard formats of every pack's carla-pack.json, and
those of its optional roadgen_asset.json, found, indexed, searched and handed back as paths."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from carla_pack_registry import CarlaPackError, Client, map_data, map_formats

from .packs import hd_pack, mini_pack

pytest.importorskip("boto3")
pytest.importorskip("moto.server")

from .conftest import ALICE, REPO, _vend  # noqa: E402


def test_a_pack_without_an_asset_file_has_its_standard_formats(tmp_path: Path) -> None:
    plain = tmp_path / "MiniPack.tar.gz"
    plain.write_bytes(mini_pack())
    assert map_formats(plain) == {"MiniTown": {}}
    standard = tmp_path / "HDPack-plain.tar.gz"
    standard.write_bytes(hd_pack(asset=None))
    formats = map_formats(standard)
    assert sorted(formats["HDTown"]) == ["navmesh", "opendrive"]
    assert formats["HDTown"]["opendrive"] == {
        "type": "file",
        "path": "Content/Maps/OpenDrive/HDTown/HDTown.xodr",
        "origin": "carla",
    }


def test_an_asset_file_adds_its_formats_and_maps(tmp_path: Path) -> None:
    tarball = tmp_path / "HDPack.tar.gz"
    tarball.write_bytes(hd_pack())
    formats = map_formats(tarball)
    assert sorted(formats) == ["HDTown", "OnlyData"]
    assert sorted(formats["HDTown"]) == ["autoware", "lanelet2", "navmesh", "opendrive", "sumo"]
    assert formats["HDTown"]["opendrive"]["origin"] == "carla"
    assert formats["HDTown"]["autoware"]["origin"] == "asset"
    assert formats["HDTown"]["autoware"]["files"]["pointcloud_map"].endswith("pointcloud_map.pcd")


def _publish_hd(w: dict[str, Any]) -> dict[str, Any]:
    tarball = w["tmp"] / "HDPack-1.0.0-carla-0.10.0-Linux.tar.gz"
    tarball.write_bytes(hd_pack())
    publisher = w["s3"]["keys"]["publisher"]
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_ACCESS_KEY_ID", publisher["access_key_id"])
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_SECRET_ACCESS_KEY", publisher["secret_access_key"])
    code, out, err = w["run"]("--json", "publish", str(tarball), "--manifest", str(w["manifest"]))
    assert code == 0, err
    return json.loads(out)


def test_published_formats_are_searched_and_handed_back_as_paths(world: dict[str, Any]) -> None:
    w = world
    entry = _publish_hd(w)
    # The repository manifest indexes the release by map and format.
    assert entry["formats"] == {
        "HDTown": ["autoware", "lanelet2", "navmesh", "opendrive", "sumo"],
        "OnlyData": ["osm"],
    }
    assert "[pack.formats]" in w["manifest"].read_text()
    _vend(w, "reader", expires_at="2099-01-01T00:00:00Z")
    w["monkeypatch"].setenv("CARLA_PACK_TOKEN", ALICE)
    run = w["run"]

    # Search by format, for the map asked for; a format it lacks finds nothing.
    code, out, err = run("find", "HDTown", "--repo", REPO, "--format", "lanelet2")
    assert code == 0 and "HDPack@1.0.0" in out, err
    code, out, err = run("find", "HDTown", "--repo", REPO, "--format", "clipgt")
    assert code == 1, out
    # A data-only map is found only when the asset index is searched too.
    code, _, _ = run("find", "OnlyData", "--repo", REPO)
    assert code == 1
    code, out, err = run("find", "OnlyData", "--repo", REPO, "--assets")
    assert code == 0 and "HDPack@1.0.0" in out, err
    code, out, err = run("ls", REPO, "--formats")
    assert code == 0 and "lanelet2" in out and "FORMATS" in out, err

    # The formats of a release, and the path of one, without a CARLA server.
    ref = f"{REPO}/HDPack"
    code, out, err = run("formats", ref)
    assert code == 0 and "autoware" in out and "Content/HDMaps/HDTown/autoware" in out, err
    code, out, err = run("data", ref, "--map-name", "HDTown", "--format", "autoware", "--file", "pointcloud_map")
    assert code == 0, err
    assert Path(out.strip()).read_bytes() == b"PCD"
    code, out, err = run("data", "HDTown", "--map", "--repo", REPO, "--format", "opendrive")
    assert code == 0 and Path(out.strip()).read_bytes() == b"<OpenDRIVE/>", err
    code, out, err = run("data", ref, "--format", "osm")
    assert code == 1 and "--map-name" in err

    # The same through the Python API, and on the unpacked folder itself.
    client = Client()
    folder = client.map_data(ref, "HDTown", "autoware")
    assert folder.is_dir() and (folder / "lanelet2_map.osm").read_bytes() == b"<osm/>"
    pack_dir = client.unpack(ref)
    assert (pack_dir / "carla-pack.json").is_file()
    assert map_data(pack_dir, "HDTown", "sumo", file="netccfg").read_bytes() == b"<configuration/>"
    assert sorted(map_formats(pack_dir)) == ["HDTown", "OnlyData"]
    with pytest.raises(CarlaPackError, match="no \"clipgt\" data"):
        client.map_data(ref, "HDTown", "clipgt")
