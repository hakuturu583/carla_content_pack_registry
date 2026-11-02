"""carla-pack: CARLA content packs shared through GitHub and S3-compatible storage.

A pack repository on GitHub lists its packs in ``carla-packs.toml`` and hands out
read-only storage credentials as a release asset; reading both with your own GitHub
token is the access check, and the pack is then downloaded straight from the bucket.
The access check, download, verification and installation are implemented in Rust
(``carla_pack_registry._core``); this package adds the ``carla-pack`` command and the
CARLA side (``deploy_pack``, ``ensure_map``).

    from carla_pack_registry import Client, ensure_map

    ensure_map(carla_client, "NishishinjukuMap", "/opt/carla/Linux/CarlaUnreal/Packs",
               repos=["tier4/carla-packs"])
    carla_client.load_world("NishishinjukuMap")
"""

from ._core import CarlaPackError, Client, __version__, publish
from .deploy import deploy_pack, ensure_map

__all__ = ["CarlaPackError", "Client", "__version__", "deploy_pack", "ensure_map", "publish"]
