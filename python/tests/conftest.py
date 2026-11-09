"""Shared fixtures: moto's S3 server with IAM authentication on, and a fake GitHub API
serving one pack repository's manifest and credential release to one token only."""

from __future__ import annotations

import json
import socket
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import pytest

from carla_pack_registry.cli import main

REPO = "tier4/carla-packs"
ALICE = "gho_alice"
BUCKET = "carla-packs"
REGION = "us-east-1"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# --------------------------------------------------------------------------- S3 (moto with IAM auth)


@pytest.fixture(scope="session")
def s3() -> dict[str, Any]:
    """A bucket and three IAM users: publisher (write), reader (read this bucket), outsider (nothing)."""
    boto3 = pytest.importorskip("boto3")
    moto_server = pytest.importorskip("moto.server")
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



def _vend(w: dict[str, Any], user: str, **extra: Any) -> None:
    w["gh"].credentials = {"version": 1, **w["s3"]["keys"][user], **extra}


