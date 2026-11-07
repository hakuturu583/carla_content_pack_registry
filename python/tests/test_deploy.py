"""deploy_pack / ensure_map: the CARLA side (mount / unmount order) with fake clients.
Downloads and archive checks are covered by the Rust tests and test_e2e.py."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import pytest

from carla_pack_registry import CarlaPackError, deploy_pack, ensure_map

RELEASE = {
    "repo": "tier4/carla-packs",
    "ref": "tier4/carla-packs/Town01Pack@1.0.0",
    "name": "Town01Pack",
    "version": "1.0.0",
    "base_release": "carla-0.10.0-Linux",
    "platform": "Linux",
    "sha256": "a" * 64,
    "maps": ["Town01_Opt"],
}


class FakeClient:
    def __init__(self, log: list[tuple[str, str]]) -> None:
        self.log = log
        self.installs: list[dict[str, Any]] = []

    def resolve(self, reference: str, **filters: Any) -> dict[str, Any]:
        self.log.append(("resolve", reference))
        return RELEASE

    def find_map(self, map_name: str, repos: Any = None, **filters: Any) -> dict[str, Any]:
        self.log.append(("find_map", map_name))
        return RELEASE

    def install(self, release: dict[str, Any], packs_dir: str, base_release: Any = None, force: bool = False) -> dict[str, Any]:
        self.log.append(("install", release["name"]))
        self.installs.append({"force": force, "packs_dir": packs_dir})
        return {
            "name": "Town01Pack",
            "version": "1.0.0",
            "base_release": "carla-0.10.0-Linux",
            "platform": "Linux",
            "maps": ["Town01_Opt"],
            "path": f"{packs_dir}/Town01Pack",
            "bytes": 1,
            "already_installed": False,
        }


@dataclass
class PackInfo:
    name: str
    mounted: bool
    maps: list[str] = field(default_factory=list)


class FakeCarla:
    def __init__(self, log: list[tuple[str, str]], packs: list[PackInfo] | None = None, in_use: bool = False) -> None:
        self.log = log
        self.packs = packs or []
        self.in_use = in_use
        self.maps = ["/Game/Carla/Maps/Town10HD_Opt"]

    def get_available_maps(self) -> list[str]:
        return self.maps

    def get_content_packs(self) -> list[PackInfo]:
        return self.packs

    def unmount_content_pack(self, name: str) -> bool:
        if self.in_use:
            raise RuntimeError(f"cannot unmount content pack '{name}': its map is the current world")
        self.log.append(("unmount", name))
        return True

    def mount_content_pack(self, path: str) -> PackInfo:
        self.log.append(("mount", path))
        self.maps = self.maps + ["/Town01Pack/Maps/Town01_Opt"]
        return PackInfo("Town01Pack", True, ["Town01_Opt"])


@pytest.fixture
def log() -> list[tuple[str, str]]:
    return []


def test_installs_without_a_server(log, tmp_path):
    pack = deploy_pack("tier4/carla-packs/Town01Pack", tmp_path, client=FakeClient(log))
    assert log == [("resolve", "tier4/carla-packs/Town01Pack"), ("install", "Town01Pack")]
    assert pack["mounted"] is False and pack["path"] == f"{tmp_path}/Town01Pack"


def test_mounts_on_the_server(log):
    carla = FakeCarla(log, [PackInfo("OtherPack", True)])
    pack = deploy_pack(RELEASE, "/srv/packs", carla_client=carla, client=FakeClient(log))
    assert log == [("install", "Town01Pack"), ("mount", "/srv/packs/Town01Pack")]
    assert pack["mounted"] is True and pack["maps"] == ["Town01_Opt"]


def test_a_mounted_pack_is_left_alone_without_force(log):
    carla = FakeCarla(log, [PackInfo("Town01Pack", True)])
    client = FakeClient(log)
    pack = deploy_pack(RELEASE, "/srv/packs", carla_client=carla, client=client)
    assert log == [("install", "Town01Pack")]  # the core no-ops (same) or refuses (other version)
    assert client.installs == [{"force": False, "packs_dir": "/srv/packs"}] and pack["mounted"] is True


def test_force_unmounts_before_the_files_are_replaced(log):
    carla = FakeCarla(log, [PackInfo("Town01Pack", True)])
    deploy_pack(RELEASE, "/srv/packs", carla_client=carla, force=True, client=FakeClient(log))
    assert log == [("unmount", "Town01Pack"), ("install", "Town01Pack"), ("mount", "/srv/packs/Town01Pack")]


def test_nothing_is_installed_when_the_server_refuses_to_unmount(log):
    carla = FakeCarla(log, [PackInfo("Town01Pack", True)], in_use=True)
    client = FakeClient(log)
    with pytest.raises(RuntimeError, match="current world"):
        deploy_pack(RELEASE, "/srv/packs", carla_client=carla, force=True, client=client)
    assert client.installs == []


def test_ensure_map(log):
    carla = FakeCarla(log)
    client = FakeClient(log)
    assert ensure_map(carla, "Town10HD_Opt", "/srv/packs", client=client) is None
    assert log == []
    pack = ensure_map(carla, "/Game/Town01Pack/Maps/Town01_Opt", "/srv/packs", client=client)
    assert pack is not None and (pack["ref"], pack["sha256"]) == (RELEASE["ref"], RELEASE["sha256"])
    assert log == [("find_map", "/Game/Town01Pack/Maps/Town01_Opt"), ("install", "Town01Pack"), ("mount", "/srv/packs/Town01Pack")]


def test_ensure_map_fails_when_the_mounted_pack_does_not_provide_the_map(log):
    carla = FakeCarla(log)
    with pytest.raises(CarlaPackError) as e:
        ensure_map(carla, "Town99", "/srv/packs", client=FakeClient(log))
    assert e.value.args[1] == "map_unavailable"
