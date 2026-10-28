"""carla-pack: client for the CARLA Content Pack Registry.

The networking, login, credential storage and download logic is implemented in Rust
(``carla_pack_registry._core``); this package provides the ``carla-pack`` command line interface.
"""

from ._core import CarlaPackError, Client, __version__

__all__ = ["CarlaPackError", "Client", "__version__"]
