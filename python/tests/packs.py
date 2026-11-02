"""Pack tarballs as CARLA's Util/ContentPacks/carla_pack.py builds them."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import tarfile

FILES = {
    "Content/Paks/Linux/MiniPack-Linux.pak": b"pak-bytes",
    "Content/Maps/OpenDrive/MiniTown.xodr": b"<OpenDRIVE/>",
}


def mini_pack(version: str = "1.0.0") -> bytes:
    """MiniPack <version>, providing the map MiniTown. Deterministic bytes per version."""
    manifest = {
        "name": "MiniPack",
        "version": version,
        "base_release": "carla-0.10.0-Linux",
        "platform": "Linux",
        "maps": [{"name": "MiniTown", "package": "/MiniPack/Maps/MiniTown"}],
        "files": {k: "sha256:" + hashlib.sha256(v).hexdigest() for k, v in FILES.items()},
    }
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as gz, tarfile.open(fileobj=gz, mode="w") as tar:
        for name, data in [("carla-pack.json", json.dumps(manifest).encode()), *FILES.items()]:
            info = tarfile.TarInfo(f"MiniPack/{name}")
            info.size = len(data)
            info.mtime = 0
            tar.addfile(info, io.BytesIO(data))
    return out.getvalue()
