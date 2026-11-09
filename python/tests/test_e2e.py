"""End to end through the `carla-pack` CLI and the Python API, with real S3 semantics:
moto's S3 server with IAM authentication on (signatures are checked, and every key only
gets what its IAM policy grants) and a fake GitHub API that serves the pack repository's
manifest and credential release to one token only.

    publisher --publish--> bucket + carla-packs.toml
    alice (can read the repo) --GitHub--> manifest + read-only key --S3--> pack
    mallory (cannot)          --GitHub--> 404
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

boto3 = pytest.importorskip("boto3")
moto_server = pytest.importorskip("moto.server")

from carla_pack_registry import CarlaPackError, Client, deploy_pack, ensure_map  # noqa: E402

from .conftest import ALICE, REPO, _vend  # noqa: E402
from .packs import big_pack, mini_pack  # noqa: E402


def _publish(w: dict[str, Any], version: str = "1.0.0") -> dict[str, Any]:
    tarball = w["tmp"] / f"MiniPack-{version}-carla-0.10.0-Linux.tar.gz"
    tarball.write_bytes(mini_pack(version))
    publisher = w["s3"]["keys"]["publisher"]
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_ACCESS_KEY_ID", publisher["access_key_id"])
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_SECRET_ACCESS_KEY", publisher["secret_access_key"])
    code, out, err = w["run"]("--json", "publish", str(tarball), "--manifest", str(w["manifest"]))
    assert code == 0, err
    return json.loads(out)


# --------------------------------------------------------------------------- tests


def test_publish_then_install_through_github_and_s3(world: dict[str, Any]) -> None:
    w = world
    entry = _publish(w)
    assert (entry["name"], entry["version"], entry["maps"]) == ("MiniPack", "1.0.0", ["MiniTown"])
    assert w["manifest"].read_text().startswith("# Packs of the test organisation")
    _vend(w, "reader", expires_at="2099-01-01T00:00:00Z")

    w["monkeypatch"].setenv("CARLA_PACK_TOKEN", ALICE)
    code, out, err = w["run"]("whoami")
    assert code == 0 and "alice" in out and "CARLA_PACK_TOKEN" in out, err
    code, out, err = w["run"]("ls", REPO)
    assert code == 0 and "tier4/carla-packs/MiniPack@1.0.0" in out and "MiniTown" in out, err

    packs = w["tmp"] / "Packs"
    code, out, err = w["run"]("--json", "install", "MiniTown", "--map", "--repo", REPO, "--packs-dir", str(packs))
    assert code == 0, err
    result = json.loads(out)
    assert result["ref"] == "tier4/carla-packs/MiniPack@1.0.0" and result["sha256"] == entry["sha256"]
    assert (packs / "MiniPack" / "Content" / "Paks" / "Linux" / "MiniPack-Linux.pak").read_bytes() == b"pak-bytes"
    assert w["gh"].leaked == []  # the GitHub token never reached the asset host

    code, out, err = w["run"]("install", f"{REPO}/MiniPack@1.0.0", "--packs-dir", str(packs))
    assert code == 0 and out.startswith("already installed"), err

    code, out, err = w["run"]("pull", f"{REPO}/MiniPack", "-o", str(w["tmp"] / "out"))
    assert code == 0, err
    assert (w["tmp"] / "out" / "MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz").read_bytes() == mini_pack("1.0.0")

    code, out, err = w["run"]("credentials", REPO)
    assert code == 0 and "2099-01-01T00:00:00Z" in out and "secret" not in out.lower(), err


def test_without_repository_access_there_is_nothing_to_download(world: dict[str, Any]) -> None:
    w = world
    _publish(w)
    _vend(w, "reader")
    w["monkeypatch"].setenv("CARLA_PACK_TOKEN", "gho_mallory")
    code, _, err = w["run"]("install", f"{REPO}/MiniPack", "--packs-dir", str(w["tmp"] / "Packs"))
    assert code == 1 and "no read access" in err
    assert not (w["tmp"] / "Packs").exists()


def test_the_bucket_enforces_the_vended_key(world: dict[str, Any]) -> None:
    w = world
    _publish(w)
    w["monkeypatch"].setenv("CARLA_PACK_TOKEN", ALICE)

    _vend(w, "outsider")  # a valid key without access to the bucket
    with pytest.raises(CarlaPackError) as e:
        Client().fetch(f"{REPO}/MiniPack")
    assert e.value.args[1] == "storage_error" and "403" in e.value.args[0]

    reader = w["s3"]["keys"]["reader"]
    w["gh"].credentials = {"version": 1, **reader, "secret_access_key": "wrong"}  # bad signature
    with pytest.raises(CarlaPackError) as e:
        Client().fetch(f"{REPO}/MiniPack")
    assert e.value.args[1] == "storage_error" and "SignatureDoesNotMatch" in e.value.args[0]

    _vend(w, "reader")
    assert Client().fetch(f"{REPO}/MiniPack").read_bytes() == mini_pack("1.0.0")


def test_publishing_never_replaces_a_release(world: dict[str, Any]) -> None:
    w = world
    _publish(w)
    code, _, err = w["run"]("publish", str(w["tmp"] / "MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz"), "--manifest", str(w["manifest"]))
    assert code == 1 and "already published" in err

    w["monkeypatch"].delenv("CARLA_PACK_UPLOAD_ACCESS_KEY_ID")
    tarball = w["tmp"] / "MiniPack-2.0.0-carla-0.10.0-Linux.tar.gz"
    tarball.write_bytes(mini_pack("2.0.0"))
    code, _, err = w["run"]("publish", str(tarball), "--manifest", str(w["manifest"]))
    assert code == 1 and "write credentials" in err


class FakeCarla:
    """The content pack part of carla.Client."""

    def __init__(self) -> None:
        self.maps = ["/Game/Carla/Maps/Town10HD_Opt"]
        self.mounted: dict[str, list[str]] = {}

    def get_available_maps(self) -> list[str]:
        return self.maps

    def get_content_packs(self) -> list[Any]:
        return [type("P", (), {"name": n, "mounted": True}) for n in self.mounted]

    def mount_content_pack(self, path: str) -> Any:
        name = Path(path).name
        manifest = json.loads((Path(path) / "carla-pack.json").read_text())
        self.mounted[name] = [m["name"] for m in manifest["maps"]]
        self.maps = self.maps + [f"/{name}/Maps/{m}" for m in self.mounted[name]]
        return type("I", (), {"mounted": True, "maps": self.mounted[name]})


def test_ensure_map_from_a_scenario_library(world: dict[str, Any]) -> None:
    w = world
    _publish(w, "1.0.0")
    v2 = _publish(w, "2.0.0")
    _vend(w, "reader")
    w["monkeypatch"].setenv("CARLA_PACK_TOKEN", ALICE)
    w["monkeypatch"].setenv("CARLA_PACK_REPOS", REPO)
    carla = FakeCarla()
    packs = w["tmp"] / "Packs"

    assert ensure_map(carla, "Town10HD_Opt", packs) is None  # the server has it: nothing happens
    pack = ensure_map(carla, "MiniTown", packs)
    assert pack is not None
    assert (pack["ref"], pack["sha256"], pack["mounted"]) == (f"{REPO}/MiniPack@2.0.0", v2["sha256"], True)
    assert ensure_map(carla, "MiniTown", packs) is None

    # A lock file pins the exact artifact: deploy_pack takes the reference and SHA-256.
    other = w["tmp"] / "Other"
    locked = deploy_pack(f"{REPO}/MiniPack", other, sha256=v2["sha256"])
    assert locked["version"] == "2.0.0" and locked["mounted"] is False
    with pytest.raises(CarlaPackError) as e:
        deploy_pack(f"{REPO}/MiniPack", other, sha256="0" * 64)
    assert e.value.args[1] == "not_found"


def test_large_packs_go_up_in_parts_and_come_back_intact(world: dict[str, Any]) -> None:
    w = world
    body = big_pack("3.0.0", 12 * 1024 * 1024)
    tarball = w["tmp"] / "MiniPack-3.0.0-carla-0.10.0-Linux.tar.gz"
    tarball.write_bytes(body)
    publisher = w["s3"]["keys"]["publisher"]
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_ACCESS_KEY_ID", publisher["access_key_id"])
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_SECRET_ACCESS_KEY", publisher["secret_access_key"])
    # 5 MiB parts: three signed UploadPart requests, then CompleteMultipartUpload.
    code, out, err = w["run"]("--json", "publish", str(tarball), "--manifest", str(w["manifest"]), "--part-size-mib", "5")
    assert code == 0, err
    assert json.loads(out)["size"] == len(body)

    _vend(w, "reader")
    w["monkeypatch"].setenv("CARLA_PACK_TOKEN", ALICE)
    packs = w["tmp"] / "Packs"
    pack = Client().install(f"{REPO}/MiniPack@3.0.0", packs)
    assert pack["version"] == "3.0.0" and (packs / "MiniPack" / "Content" / "padding.bin").stat().st_size == 12 * 1024 * 1024
