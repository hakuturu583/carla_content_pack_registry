"""One call from a registry bucket to a pack mounted on a running CARLA server.

The download, archive checks and installation are done by the Rust core
(``Client.install_pack``); this module only adds the CARLA side, through the
``carla.Client`` content pack API (``get_content_packs`` / ``mount_content_pack`` /
``unmount_content_pack``). ``carla`` itself is not imported: pass your client.
"""

from __future__ import annotations

import os
from typing import Any

from ._core import CarlaPackError, Client


def deploy_pack(
    bucket: str,
    key: str,
    packs_dir: str | os.PathLike[str],
    *,
    carla_client: Any = None,
    base_release: str | None = None,
    force: bool = False,
    client: Client | None = None,
) -> dict[str, Any]:
    """Downloads the pack tarball ``key`` from ``bucket`` (OWNER/NAME, NAME or id), verifies
    every file against its manifest, installs it as ``<packs_dir>/<name>`` and, when
    ``carla_client`` is given, mounts it on that server.

    ``packs_dir`` must be readable by the CARLA server under the same path (the package's
    ``Linux/CarlaUnreal/Packs``, a ``-carla-packs`` directory, or any folder when mounting).
    ``base_release`` refuses a pack built for another release before anything is installed;
    the server checks it again when mounting. ``force`` replaces an installed pack of the
    same name and unmounts a mounted one before its files are replaced (the server refuses
    while its content is in use, and then nothing is installed: load another map or restart).
    ``client`` defaults to ``Client()`` (saved login / environment).

    Returns the installed pack: {name, version, base_release, platform, maps, path, bytes,
    mounted}.
    """
    client = client or Client()
    bucket_id = client.resolve_bucket(bucket)["id"]
    if carla_client is not None:
        # Unmount before the files are replaced. The name is known from the key when it
        # follows the <Pack>-<version>-<release>.tar.gz convention (a pack name has no "-").
        _release(carla_client, key.rsplit("/", 1)[-1].split("-", 1)[0], force)
    pack = client.install_pack(bucket_id, key, os.fspath(packs_dir), base_release=base_release, force=force)
    pack["mounted"] = False
    if carla_client is None:
        return pack

    _release(carla_client, pack["name"], force)  # a key named otherwise
    info = carla_client.mount_content_pack(pack["path"])
    pack["mounted"] = bool(info.mounted)
    pack["maps"] = list(info.maps)
    return pack


def _release(carla_client: Any, name: str, force: bool) -> None:
    """Unmounts the mounted pack `name` when `force`; refuses otherwise."""
    if not any(p.name == name and p.mounted for p in carla_client.get_content_packs()):
        return
    if not force:
        raise CarlaPackError(
            f"a pack named {name} is already mounted on the server (force unmounts it first)", "pack_mounted"
        )
    carla_client.unmount_content_pack(name)
