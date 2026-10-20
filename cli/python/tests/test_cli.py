"""End-to-end tests of the carla-pack CLI against a local registry in mock-auth mode
(wrangler dev + SeaweedFS, see the repository README). Skipped when it is not running.

    CARLA_PACK_TEST_REGISTRY=http://localhost:8787 pytest
"""

from __future__ import annotations

import json
import os
import urllib.request
import uuid
from pathlib import Path

import pytest

from carla_pack.cli import CliError, _safe_dest, main

REGISTRY = os.environ.get("CARLA_PACK_TEST_REGISTRY", "http://localhost:8787")
ALICE_SECRET = "local-alice-secret-not-for-production"


def _registry_up() -> bool:
    try:
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(f"{REGISTRY}/cli/config", timeout=3) as r:
            return json.load(r).get("authMode") == "mock"
    except OSError:
        return False


needs_registry = pytest.mark.skipif(not _registry_up(), reason=f"no mock-mode registry at {REGISTRY}")


@pytest.fixture
def cli(tmp_path, monkeypatch, capsys):
    """Runs `carla-pack ...` in-process with an isolated config dir; returns (exit, stdout, stderr)."""
    monkeypatch.setenv("CARLA_PACK_CONFIG_DIR", str(tmp_path / "config"))
    monkeypatch.setenv("CARLA_PACK_REGISTRY", REGISTRY)
    monkeypatch.delenv("CARLA_PACK_TOKEN", raising=False)
    monkeypatch.setenv("NO_PROXY", "localhost,127.0.0.1")

    def run(*argv: str) -> tuple[int, str, str]:
        code = main(list(argv))
        out = capsys.readouterr()
        return code, out.out, out.err

    return run


def test_safe_dest_refuses_escaping_keys(tmp_path):
    assert _safe_dest(tmp_path, "maps/a.tar") == tmp_path / "maps" / "a.tar"
    for bad in ["../x", "maps/../../x", "/", ".", ""]:
        with pytest.raises(CliError):
            _safe_dest(tmp_path, bad)


def test_requires_login(cli):
    code, _, err = cli("ls")
    assert code == 1 and "carla-pack login" in err


@needs_registry
def test_mock_registry_has_no_device_login(cli):
    code, _, err = cli("login")
    assert code == 1 and "--token mock:alice" in err


@needs_registry
def test_owner_registers_shares_and_reader_downloads(cli, monkeypatch, tmp_path):
    name = f"cli-{uuid.uuid4().hex[:8]}"
    ref = f"alice/{name}"

    assert cli("login", "--token", "mock:alice")[0] == 0
    assert (tmp_path / "config" / "credentials.json").stat().st_mode & 0o777 == 0o600
    code, out, _ = cli("--json", "whoami")
    assert code == 0 and json.loads(out)["user"]["login"] == "alice"

    monkeypatch.setenv("CARLA_PACK_SECRET_ACCESS_KEY", ALICE_SECRET)
    code, out, err = cli(
        "bucket", "add", name, "--provider", "local", "--bucket", "alice-packs", "--access-key-id", "LOCALALICEKEY00001"
    )
    assert code == 0, err
    assert ALICE_SECRET not in out
    code, out, _ = cli("bucket", "verify", ref)
    assert code == 0 and "verified" in out

    code, out, _ = cli("acl", "add", ref, "--team", "carla-sim/maps", "--user", "bob")
    assert code == 0 and "carla-sim/maps" in out and "bob" in out
    code, out, _ = cli("acl", "rm", ref, "--team", "carla-sim/maps")
    assert code == 0 and "carla-sim/maps" not in out

    # bob reads through the user ACL entry; carol cannot see the bucket at all
    assert cli("login", "--token", "mock:bob")[0] == 0
    code, out, _ = cli("--json", "ls")
    assert ref in [f"{b['owner']['login']}/{b['name']}" for b in json.loads(out)]
    code, out, _ = cli("ls", ref, "maps/")
    assert "maps/Town10HD_Opt.tar.zst" in out and "vehicles/" not in out

    dest = tmp_path / "out"
    code, out, err = cli("pull", ref, "maps/", "vehicles/sedan with space.tar.zst", "-o", str(dest))
    assert code == 0, err
    assert (dest / "maps" / "readme.md").read_text() == "# Alice maps\n"
    assert (dest / "vehicles" / "sedan with space.tar.zst").read_text() == "fake vehicle pack\n"
    code, out, _ = cli("pull", ref, "maps/readme.md", "-o", str(dest))
    assert "skip" in out
    assert not list(dest.rglob("*.part"))

    code, _, err = cli("bucket", "rm", ref, "--yes")
    assert code == 1 and "owner" in err  # bob cannot manage alice's bucket

    assert cli("login", "--token", "mock:carol")[0] == 0
    code, _, err = cli("ls", ref)
    assert code == 1 and "no readable bucket" in err

    assert cli("login", "--token", "mock:alice")[0] == 0
    code, out, _ = cli("bucket", "rm", ref, "--yes")
    assert code == 0 and "untouched" in out
    code, out, _ = cli("logout")
    assert code == 0
    assert cli("whoami")[0] == 1
