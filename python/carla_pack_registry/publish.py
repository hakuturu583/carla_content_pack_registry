"""Publishing a pack: upload the tarball, then append its entry to carla-packs.toml.

S3-compatible buckets are uploaded to by the Rust core (multipart, signed). The Hugging
Face Hub is uploaded to through huggingface_hub, its own client (its large-file transfer
protocol is not something to reimplement): ``pip install 'carla-pack-registry[huggingface]'``,
and a write token from ``HF_TOKEN`` or ``hf auth login``.
"""

from __future__ import annotations

import os
from typing import Any

from . import _core
from ._core import CarlaPackError


def publish(
    manifest: str | os.PathLike[str],
    tarball: str | os.PathLike[str],
    *,
    key: str | None = None,
    upload: bool = True,
    overwrite: bool = False,
    description: str | None = None,
    part_size: int | None = None,
) -> dict[str, Any]:
    """Uploads the pack ``tarball`` to the storage named in the manifest at ``manifest`` (a
    checkout of the pack repository) and appends its entry; commit and push the manifest
    to publish it. Returns the new entry.

    Everything is checked before anything is uploaded: a release with the same name,
    version, base release and platform is refused, and so is an existing file under the
    same path unless ``overwrite``. ``upload=False`` only appends the entry (the file was
    uploaded some other way). ``part_size`` is the S3 multipart part size in bytes.
    """
    plan = _core.plan_publish(manifest, tarball, key=key, description=description)
    storage = plan["storage"]
    if not upload or storage["type"] != "huggingface":
        return _core.publish(
            manifest,
            tarball,
            key=key,
            upload=upload,
            overwrite=overwrite,
            description=description,
            part_size=part_size,
        )
    _upload_to_hugging_face(storage, plan["object_key"], os.fspath(tarball), plan["entry"], overwrite)
    _core.append_entry(manifest, plan["entry"])
    return plan["entry"]


def _upload_to_hugging_face(
    storage: dict[str, Any], path: str, tarball: str, entry: dict[str, Any], overwrite: bool
) -> None:
    try:
        from huggingface_hub import HfApi
    except ImportError:
        raise CarlaPackError(
            "uploading to Hugging Face needs huggingface_hub: pip install 'carla-pack-registry[huggingface]' "
            "(or upload yourself and publish with --no-upload)",
            "usage",
        ) from None
    api = HfApi(endpoint=storage["endpoint"])
    where = dict(repo_id=storage["repo"], repo_type=storage["repo_type"], revision=storage["revision"])
    try:
        if not overwrite and api.file_exists(filename=path, **where):
            raise CarlaPackError(
                f"{path} already exists in {storage['repo']}@{storage['revision']}; pass overwrite to replace it",
                "usage",
            )
        api.upload_file(
            path_or_fileobj=tarball,
            path_in_repo=path,
            commit_message=f"Add {entry['name']} {entry['version']} ({entry['base_release']}, {entry['platform']})",
            **where,
        )
    except CarlaPackError:
        raise
    except Exception as e:  # huggingface_hub raises its own HTTP / auth errors
        raise CarlaPackError(
            f"uploading {path} to Hugging Face {storage['repo_type']} {storage['repo']} failed: {e} "
            "(a write token comes from HF_TOKEN or `hf auth login`; the revision must be a branch)",
            "storage_error",
        ) from e
