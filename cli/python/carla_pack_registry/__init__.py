"""carla-pack: client for the CARLA Content Pack Registry.

The networking, login, credential storage, download and pack installation logic is
implemented in Rust (``carla_pack_registry._core``); this package provides the
``carla-pack`` command line interface and ``deploy_pack``, which also mounts the pack
on a running CARLA server.
"""

from ._core import CarlaPackError, Client, __version__
from .deploy import deploy_pack

__all__ = ["CarlaPackError", "Client", "__version__", "deploy_pack"]
