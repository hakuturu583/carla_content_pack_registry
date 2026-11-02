"""From a pack repository to a pack mounted on a running CARLA server.

The access check, download, checksum and archive checks are done by the Rust core
(``Client.install``); this module adds the CARLA side through the ``carla.Client``
content pack API (``get_available_maps`` / ``get_content_packs`` /
``mount_content_pack`` / ``unmount_content_pack``). ``carla`` itself is not imported:
pass your client (the official one or ``typesafe_carla.carla``).
"""

from __future__ import annotations

import os
from typing import Any, Union

from ._core import CarlaPackError, Client

Release = Union[str, "dict[str, Any]"]
"""A reference ``OWNER/REPO/PACK[@VERSION]`` or a release from ``Client.resolve`` / ``find_map``."""


def deploy_pack(
    release: Release,
    packs_dir: str | os.PathLike[str],
    *,
    carla_client: Any = None,
    base_release: str | None = None,
    platform: str | None = None,
    sha256: str | None = None,
    force: bool = False,
    client: Client | None = None,
) -> dict[str, Any]:
    """Installs ``release`` as ``<packs_dir>/<name>`` and, when ``carla_client`` is given,
    mounts it on that server.

    The pack is downloaded only when it is not installed already (same name, version,
    base release and platform), and is verified against both the repository manifest's
    SHA-256 and the pack's own ``carla-pack.json`` before anything is moved into place.

    ``packs_dir`` must be readable by the CARLA server under the same path (the package's
    ``Linux/CarlaUnreal/Packs``, a ``-carla-packs`` directory, or any folder when mounting).
    ``base_release`` refuses a pack built for another release; the server checks it again
    when mounting. ``sha256`` pins one exact artifact (for lock files). ``force`` replaces
    another installed version of the pack, unmounting it first (the server refuses while its
    content is in use; then nothing is installed: load another map or restart).
    ``client`` defaults to ``Client()``.

    Returns the installed pack: {name, version, base_release, platform, maps, path, bytes,
    already_installed, mounted}.
    """
    client = client or Client()
    if isinstance(release, str):
        release = client.resolve(release, base_release=base_release, platform=platform, sha256=sha256)
    name = release["name"]
    if carla_client is not None and _is_mounted(carla_client, name):
        if not force:
            # The same pack: nothing to do. Another version: refused (force replaces it).
            pack = client.install(release, os.fspath(packs_dir), base_release=base_release)
            pack["mounted"] = True
            return pack
        carla_client.unmount_content_pack(name)  # before its files are replaced
    pack = client.install(release, os.fspath(packs_dir), base_release=base_release, force=force)
    pack["mounted"] = False
    if carla_client is None:
        return pack
    info = carla_client.mount_content_pack(str(pack["path"]))
    pack["mounted"] = bool(info.mounted)
    pack["maps"] = list(info.maps) or pack["maps"]
    return pack


def ensure_map(
    carla_client: Any,
    map_name: str,
    packs_dir: str | os.PathLike[str],
    *,
    repos: list[str] | None = None,
    base_release: str | None = None,
    platform: str | None = None,
    sha256: str | None = None,
    client: Client | None = None,
) -> dict[str, Any] | None:
    """Makes the CARLA map ``map_name`` available on the server behind ``carla_client``.

    Returns ``None`` when the server already has it. Otherwise finds the newest pack
    providing it in ``repos`` (default: CARLA_PACK_REPOS / ``carla-pack repo add``) that the
    current GitHub user can read, installs it into ``packs_dir`` and mounts it, and returns
    the installed pack (see :func:`deploy_pack`). ``sha256`` pins the exact pack, as a lock
    file would. Then ``carla_client.load_world(map_name)`` works.
    """
    if _has_map(carla_client, map_name):
        return None
    client = client or Client()
    release = client.find_map(map_name, repos, base_release=base_release, platform=platform, sha256=sha256)
    pack = deploy_pack(release, packs_dir, carla_client=carla_client, base_release=base_release, client=client)
    pack["ref"] = release["ref"]
    pack["sha256"] = release["sha256"]
    if not _has_map(carla_client, map_name):
        raise CarlaPackError(
            f"{release['ref']} was mounted but the server still does not list map {map_name}",
            "map_unavailable",
        )
    return pack


def _short(name: str) -> str:
    return name.rsplit("/", 1)[-1]


def _has_map(carla_client: Any, map_name: str) -> bool:
    wanted = _short(map_name)
    return any(m == map_name or _short(m) == wanted for m in carla_client.get_available_maps())


def _is_mounted(carla_client: Any, name: str) -> bool:
    return any(p.name == name and p.mounted for p in carla_client.get_content_packs())
