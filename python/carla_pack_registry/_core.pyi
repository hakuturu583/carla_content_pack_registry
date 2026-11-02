from os import PathLike
from pathlib import Path
from typing import Any, Callable

__version__: str

class CarlaPackError(Exception):
    """args = (message, code)"""

class Client:
    def __init__(self, token: str | None = None, repos: list[str] | None = None) -> None: ...
    @property
    def token_source(self) -> str | None: ...
    @property
    def repos(self) -> list[str]: ...
    @property
    def cache_dir(self) -> Path: ...
    def whoami(self) -> dict[str, Any]: ...
    def list_packs(self, repo: str) -> list[dict[str, Any]]: ...
    def resolve(
        self,
        reference: str,
        base_release: str | None = None,
        platform: str | None = None,
        sha256: str | None = None,
    ) -> dict[str, Any]: ...
    def find_map(
        self,
        map_name: str,
        repos: list[str] | None = None,
        base_release: str | None = None,
        platform: str | None = None,
        sha256: str | None = None,
    ) -> dict[str, Any]: ...
    def fetch(
        self,
        release: str | dict[str, Any],
        base_release: str | None = None,
        platform: str | None = None,
        sha256: str | None = None,
    ) -> Path: ...
    def install(
        self,
        release: str | dict[str, Any],
        packs_dir: str | PathLike[str],
        base_release: str | None = None,
        platform: str | None = None,
        sha256: str | None = None,
        force: bool = False,
    ) -> dict[str, Any]: ...
    def credentials_status(self, repo: str) -> dict[str, Any]: ...

def publish(
    manifest: str | PathLike[str],
    tarball: str | PathLike[str],
    key: str | None = None,
    upload: bool = True,
    overwrite: bool = False,
    description: str | None = None,
    part_size: int | None = None,
) -> dict[str, Any]: ...
def device_login(client_id: str, scope: str, on_code: Callable[[str, str], None]) -> str: ...
def save_login(token: str, login: str) -> None: ...
def remove_login() -> bool: ...
def saved_repos() -> list[str]: ...
def add_repo(repo: str) -> bool: ...
def remove_repo(repo: str) -> bool: ...
