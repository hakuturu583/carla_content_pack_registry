"""Publishing to the Hugging Face Hub through huggingface_hub. The Hub is not reachable from
tests, so HfApi's two network calls are replaced -- after checking that every call
carla-pack makes binds to the real signatures of the installed huggingface_hub."""

from __future__ import annotations

import inspect
import sys
from pathlib import Path
from typing import Any

import pytest

from carla_pack_registry import CarlaPackError, publish

from .packs import mini_pack

hf = pytest.importorskip("huggingface_hub")

MANIFEST = """# public packs
schema = 1

[storage]
type = "huggingface"
repo = "AutowareFoundation/carla-packs"
revision = "main"
prefix = "packs/"
"""


@pytest.fixture
def hub(monkeypatch: pytest.MonkeyPatch) -> dict[str, Any]:
    state: dict[str, Any] = {"files": set(), "uploads": [], "endpoints": []}
    real_init = hf.HfApi.__init__

    def init(self: Any, *args: Any, **kwargs: Any) -> None:
        inspect.signature(real_init).bind(self, *args, **kwargs)
        state["endpoints"].append(kwargs.get("endpoint"))
        real_init(self, *args, **kwargs)

    def file_exists(self: Any, *args: Any, **kwargs: Any) -> bool:
        bound = inspect.signature(real["file_exists"]).bind(self, *args, **kwargs)
        return (bound.arguments["repo_id"], bound.arguments["filename"]) in state["files"]

    def upload_file(self: Any, *args: Any, **kwargs: Any) -> None:
        inspect.signature(real["upload_file"]).bind(self, *args, **kwargs)
        if state.get("fail"):
            raise hf.errors.HfHubHTTPError("401 Unauthorized: invalid token")
        state["uploads"].append({**kwargs, "bytes": Path(kwargs["path_or_fileobj"]).read_bytes()})
        state["files"].add((kwargs["repo_id"], kwargs["path_in_repo"]))

    real = {"file_exists": hf.HfApi.file_exists, "upload_file": hf.HfApi.upload_file}
    monkeypatch.setattr(hf.HfApi, "__init__", init)
    monkeypatch.setattr(hf.HfApi, "file_exists", file_exists)
    monkeypatch.setattr(hf.HfApi, "upload_file", upload_file)
    return state


@pytest.fixture
def repo(tmp_path: Path) -> dict[str, Path]:
    manifest = tmp_path / "carla-packs.toml"
    manifest.write_text(MANIFEST)
    tarball = tmp_path / "MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz"
    tarball.write_bytes(mini_pack("1.0.0"))
    return {"manifest": manifest, "tarball": tarball}


def test_publish_uploads_to_the_hub_and_appends_the_entry(hub: dict[str, Any], repo: dict[str, Path]) -> None:
    entry = publish(repo["manifest"], repo["tarball"])
    (upload,) = hub["uploads"]
    assert upload["repo_id"] == "AutowareFoundation/carla-packs"
    assert (upload["repo_type"], upload["revision"]) == ("dataset", "main")
    assert upload["path_in_repo"] == "packs/MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz"
    assert upload["commit_message"] == "Add MiniPack 1.0.0 (carla-0.10.0-Linux, Linux)"
    assert upload["bytes"] == repo["tarball"].read_bytes()
    assert hub["endpoints"] == ["https://huggingface.co"]

    text = repo["manifest"].read_text()
    assert text.startswith("# public packs") and f'sha256 = "{entry["sha256"]}"' in text

    # The same release again: refused before anything is uploaded.
    with pytest.raises(CarlaPackError, match="already published"):
        publish(repo["manifest"], repo["tarball"])
    assert len(hub["uploads"]) == 1


def test_an_existing_file_is_not_replaced_without_overwrite(hub: dict[str, Any], repo: dict[str, Path]) -> None:
    hub["files"].add(("AutowareFoundation/carla-packs", "packs/MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz"))
    with pytest.raises(CarlaPackError, match="already exists"):
        publish(repo["manifest"], repo["tarball"])
    assert hub["uploads"] == [] and "[[pack]]" not in repo["manifest"].read_text()
    publish(repo["manifest"], repo["tarball"], overwrite=True)
    assert len(hub["uploads"]) == 1


def test_a_failed_upload_adds_no_entry(hub: dict[str, Any], repo: dict[str, Path]) -> None:
    hub["fail"] = True
    with pytest.raises(CarlaPackError) as e:
        publish(repo["manifest"], repo["tarball"])
    assert e.value.args[1] == "storage_error" and "HF_TOKEN" in e.value.args[0]
    assert "[[pack]]" not in repo["manifest"].read_text()


def test_no_upload_only_appends(hub: dict[str, Any], repo: dict[str, Path]) -> None:
    publish(repo["manifest"], repo["tarball"], upload=False)
    assert hub["uploads"] == [] and "[[pack]]" in repo["manifest"].read_text()


def test_without_huggingface_hub_the_error_says_what_to_install(repo: dict[str, Path], monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setitem(sys.modules, "huggingface_hub", None)  # import fails
    with pytest.raises(CarlaPackError, match=r"carla-pack-registry\[huggingface\]"):
        publish(repo["manifest"], repo["tarball"])
    assert "[[pack]]" not in repo["manifest"].read_text()
