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

A pack's map data by format, without a CARLA server: the standard formats every pack
names in its ``carla-pack.json`` (``opendrive``, ``navmesh``, ``traffic_manager``,
``map_logic``), and those of its optional ``Content/Config/roadgen_asset.json``
(``lanelet2``, ``autoware``, ``pointcloud``, ``sumo``, ``commonroad``, ``clipgt``, …):

    client = Client()
    lanelet2 = client.map_data("hakuturu583/carla-packs-test/RoadgenHighwayPack",
                               "RoadgenHighway", "lanelet2")
    autoware = client.map_data(release, "RoadgenHighway", "autoware")   # the folder
    client.formats(release)                       # {map: {format: {type, path, files, origin}}}
    map_formats("/opt/carla/CarlaUnreal/Packs/RoadgenHighwayPack")   # an installed pack
    map_data(pack_dir, "RoadgenHighway", "autoware", file="pointcloud_map")
"""

from ._core import CarlaPackError, Client, __version__, map_data, map_formats
from .deploy import deploy_pack, ensure_map
from .publish import publish

__all__ = [
    "CarlaPackError",
    "Client",
    "__version__",
    "deploy_pack",
    "ensure_map",
    "map_data",
    "map_formats",
    "publish",
]
