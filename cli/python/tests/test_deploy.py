"""deploy_pack: the CARLA side (mount / unmount) with fake registry and CARLA clients.
The download and archive checks are covered by the Rust tests (cli/tests/pack.rs)."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import pytest

from carla_pack_registry import CarlaPackError, deploy_pack


class FakeRegistry:
    def __init__(self, log: list[tuple[str, str]] | None = None) -> None:
        self.installs: list[tuple[str, str, str, str | None, bool]] = []
        self.log = log if log is not None else []

    def resolve_bucket(self, reference: str) -> dict[str, Any]:
        assert reference == "alice/maps"
        return {"id": "bucket-id"}

    def install_pack(self, id: str, key: str, packs_dir: str, base_release=None, force=False) -> dict[str, Any]:
        self.installs.append((id, key, packs_dir, base_release, force))
        self.log.append(("install", key))
        return {
            "name": "Town01Pack",
            "version": "1.0.0",
            "base_release": "carla-0.10.0-Linux",
            "platform": "Linux",
            "maps": ["Town01_Opt"],
            "path": f"{packs_dir}/Town01Pack",
            "bytes": 1,
        }


@dataclass
class PackInfo:
    name: str
    mounted: bool
    maps: list[str] = field(default_factory=list)


class FakeCarla:
    def __init__(self, packs: list[PackInfo] | None = None, in_use: bool = False) -> None:
        self.packs = packs or []
        self.in_use = in_use
        self.calls: list[tuple[str, str]] = []

    def get_content_packs(self) -> list[PackInfo]:
        return self.packs

    def unmount_content_pack(self, name: str) -> bool:
        if self.in_use:
            raise RuntimeError(f"cannot unmount content pack '{name}': its map is the current world")
        self.calls.append(("unmount", name))
        for p in self.packs:
            if p.name == name:
                p.mounted = False
        return True

    def mount_content_pack(self, path: str) -> PackInfo:
        self.calls.append(("mount", path))
        return PackInfo("Town01Pack", True, ["Town01_Opt"])


def test_installs_without_a_server(tmp_path):
    reg = FakeRegistry()
    pack = deploy_pack("alice/maps", "packs/T.tar.gz", tmp_path, base_release="carla-0.10.0-Linux", client=reg)
    assert reg.installs == [("bucket-id", "packs/T.tar.gz", str(tmp_path), "carla-0.10.0-Linux", False)]
    assert pack["mounted"] is False and pack["path"] == f"{tmp_path}/Town01Pack"


def test_mounts_on_the_server():
    carla = FakeCarla([PackInfo("OtherPack", True)])
    pack = deploy_pack("alice/maps", "k", "/srv/packs", carla_client=carla, client=FakeRegistry())
    assert carla.calls == [("mount", "/srv/packs/Town01Pack")]
    assert pack["mounted"] is True and pack["maps"] == ["Town01_Opt"]


def test_a_mounted_pack_of_the_same_name_needs_force():
    carla = FakeCarla([PackInfo("Town01Pack", True)])
    with pytest.raises(CarlaPackError) as e:
        deploy_pack("alice/maps", "k", "/srv/packs", carla_client=carla, client=FakeRegistry())
    assert e.value.args[1] == "pack_mounted" and carla.calls == []

    deploy_pack("alice/maps", "k", "/srv/packs", carla_client=carla, force=True, client=FakeRegistry())
    assert carla.calls == [("unmount", "Town01Pack"), ("mount", "/srv/packs/Town01Pack")]


def test_an_unmounted_pack_of_the_same_name_is_just_mounted():
    carla = FakeCarla([PackInfo("Town01Pack", False)])
    deploy_pack("alice/maps", "k", "/srv/packs", carla_client=carla, client=FakeRegistry())
    assert carla.calls == [("mount", "/srv/packs/Town01Pack")]


KEY = "packs/carla-0.10.0-Linux/Town01Pack-1.0.0-carla-0.10.0-Linux.tar.gz"


def test_force_unmounts_before_the_files_are_replaced():
    carla = FakeCarla([PackInfo("Town01Pack", True)])
    reg = FakeRegistry(carla.calls)
    deploy_pack("alice/maps", KEY, "/srv/packs", carla_client=carla, force=True, client=reg)
    assert carla.calls == [("unmount", "Town01Pack"), ("install", KEY), ("mount", "/srv/packs/Town01Pack")]


def test_nothing_is_installed_when_the_server_refuses_to_unmount():
    carla = FakeCarla([PackInfo("Town01Pack", True)], in_use=True)
    reg = FakeRegistry()
    with pytest.raises(RuntimeError, match="current world"):
        deploy_pack("alice/maps", KEY, "/srv/packs", carla_client=carla, force=True, client=reg)
    assert reg.installs == []

    with pytest.raises(CarlaPackError) as e:  # without force: refused before downloading
        deploy_pack("alice/maps", KEY, "/srv/packs", carla_client=carla, client=reg)
    assert e.value.args[1] == "pack_mounted" and reg.installs == []
