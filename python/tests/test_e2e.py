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
import socket
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import pytest

boto3 = pytest.importorskip("boto3")
moto_server = pytest.importorskip("moto.server")

from carla_pack_registry import CarlaPackError, Client, deploy_pack, ensure_map  # noqa: E402
from carla_pack_registry.cli import main  # noqa: E402

from .packs import big_pack, mini_pack  # noqa: E402

REPO = "tier4/carla-packs"
ALICE = "gho_alice"
BUCKET = "carla-packs"
REGION = "us-east-1"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# --------------------------------------------------------------------------- S3 (moto with IAM auth)


@pytest.fixture(scope="module")
def s3() -> dict[str, Any]:
    """A bucket and three IAM users: publisher (write), reader (read this bucket), outsider (nothing)."""
    port = _free_port()
    server = moto_server.ThreadedMotoServer(ip_address="127.0.0.1", port=port, verbose=False)
    server.start()
    endpoint = f"http://127.0.0.1:{port}"
    root = {"aws_access_key_id": "root", "aws_secret_access_key": "root", "region_name": REGION}
    keys: dict[str, dict[str, str]] = {}

    def authenticate_after(requests: float) -> None:
        # The server runs in this process, so moto's own switch can be flipped directly.
        from moto import settings
        from moto.core.responses import ActionAuthenticatorMixin

        settings.INITIAL_NO_AUTH_ACTION_COUNT = requests
        ActionAuthenticatorMixin.request_count = 0

    authenticate_after(10_000)  # setup runs unauthenticated
    if True:
        iam = boto3.client("iam", endpoint_url=endpoint, **root)
        boto3.client("s3", endpoint_url=endpoint, **root).create_bucket(Bucket=BUCKET)
        policies = {
            "publisher": ["s3:GetObject", "s3:PutObject", "s3:ListBucket"],
            "reader": ["s3:GetObject"],
            "outsider": ["s3:ListAllMyBuckets"],
        }
        for user, actions in policies.items():
            iam.create_user(UserName=user)
            iam.put_user_policy(
                UserName=user,
                PolicyName="p",
                PolicyDocument=json.dumps(
                    {
                        "Version": "2012-10-17",
                        "Statement": [
                            {
                                "Effect": "Allow",
                                "Action": actions,
                                "Resource": [f"arn:aws:s3:::{BUCKET}", f"arn:aws:s3:::{BUCKET}/*"],
                            }
                        ],
                    }
                ),
            )
            k = iam.create_access_key(UserName=user)["AccessKey"]
            keys[user] = {"access_key_id": k["AccessKeyId"], "secret_access_key": k["SecretAccessKey"]}
    authenticate_after(0)  # from now on every request is authenticated and authorised
    yield {"endpoint": endpoint, "keys": keys}
    server.stop()


# --------------------------------------------------------------------------- fake GitHub


class FakeGitHub:
    """Serves REPO's manifest and credential release to ALICE only, like a private repository."""

    def __init__(self, manifest: Path) -> None:
        self.manifest = manifest
        self.credentials: dict[str, Any] | None = None
        self.leaked: list[str] = []  # Authorization headers that reached the asset host
        self.api = self._serve(self._api_handler())
        self.assets = self._serve(self._asset_handler())

    @staticmethod
    def _serve(handler: type[BaseHTTPRequestHandler]) -> ThreadingHTTPServer:
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        return server

    @staticmethod
    def url(server: ThreadingHTTPServer) -> str:
        return f"http://127.0.0.1:{server.server_address[1]}"

    def _api_handler(self) -> type[BaseHTTPRequestHandler]:
        gh = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args: Any) -> None:
                pass

            def send(self, status: int, body: bytes, ctype: str = "application/json", **headers: str) -> None:
                self.send_response(status)
                self.send_header("content-type", ctype)
                self.send_header("content-length", str(len(body)))
                for k, v in headers.items():
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                if self.headers.get("authorization") != f"Bearer {ALICE}":
                    status = 401 if self.path == "/user" else 404
                    return self.send(status, b'{"message":"Not Found"}')
                base = f"/repos/{REPO}"
                if self.path == "/user":
                    return self.send(200, json.dumps({"login": "alice", "id": 1001}).encode())
                if self.path == f"{base}/contents/carla-packs.toml":
                    return self.send(200, gh.manifest.read_bytes(), "text/plain")
                if self.path == f"{base}/releases/tags/carla-pack-credentials" and gh.credentials:
                    asset = {"name": "credentials.json", "url": f"{gh.url(gh.api)}{base}/releases/assets/7"}
                    return self.send(200, json.dumps({"assets": [asset]}).encode())
                if self.path == f"{base}/releases/assets/7" and gh.credentials:
                    return self.send(302, b"", location=f"{gh.url(gh.assets)}/signed/credentials.json")
                return self.send(404, b'{"message":"Not Found"}')

        return Handler

    def _asset_handler(self) -> type[BaseHTTPRequestHandler]:
        gh = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args: Any) -> None:
                pass

            def do_GET(self) -> None:
                if self.headers.get("authorization"):
                    gh.leaked.append(self.headers["authorization"])
                body = json.dumps(gh.credentials).encode()
                self.send_response(200)
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        return Handler

    def close(self) -> None:
        for s in (self.api, self.assets):
            s.shutdown()
            s.server_close()


@pytest.fixture
def world(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, s3: dict[str, Any], capsys: pytest.CaptureFixture[str]):
    manifest = tmp_path / "repo" / "carla-packs.toml"
    manifest.parent.mkdir()
    manifest.write_text(
        "# Packs of the test organisation\nschema = 1\n\n[storage]\n"
        f'endpoint = "{s3["endpoint"]}"\nregion = "{REGION}"\nbucket = "{BUCKET}"\nprefix = "packs/{uuid.uuid4().hex}/"\n'
        'credentials = "github-release"\n'
    )
    gh = FakeGitHub(manifest)
    for var in ("CARLA_PACK_TOKEN", "GH_TOKEN", "GITHUB_TOKEN", "CARLA_PACK_REPOS", "AWS_ACCESS_KEY_ID"):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.setenv("CARLA_PACK_NO_GH", "1")
    monkeypatch.setenv("CARLA_PACK_CONFIG_DIR", str(tmp_path / "config"))
    monkeypatch.setenv("CARLA_PACK_CACHE_DIR", str(tmp_path / "cache"))
    monkeypatch.setenv("CARLA_PACK_GITHUB_API", gh.url(gh.api))

    def run(*argv: str) -> tuple[int, str, str]:
        code = main(list(argv))
        out = capsys.readouterr()
        return code, out.out, out.err

    yield {"tmp": tmp_path, "manifest": manifest, "gh": gh, "s3": s3, "run": run, "monkeypatch": monkeypatch}
    gh.close()


def _publish(w: dict[str, Any], version: str = "1.0.0") -> dict[str, Any]:
    tarball = w["tmp"] / f"MiniPack-{version}-carla-0.10.0-Linux.tar.gz"
    tarball.write_bytes(mini_pack(version))
    publisher = w["s3"]["keys"]["publisher"]
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_ACCESS_KEY_ID", publisher["access_key_id"])
    w["monkeypatch"].setenv("CARLA_PACK_UPLOAD_SECRET_ACCESS_KEY", publisher["secret_access_key"])
    code, out, err = w["run"]("--json", "publish", str(tarball), "--manifest", str(w["manifest"]))
    assert code == 0, err
    return json.loads(out)


def _vend(w: dict[str, Any], user: str, **extra: Any) -> None:
    w["gh"].credentials = {"version": 1, **w["s3"]["keys"][user], **extra}


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
