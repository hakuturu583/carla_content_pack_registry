"""`carla-pack` command line interface: argument parsing, prompts and output only.
The work (GitHub access check, downloads, verification, installation, uploads) is done
by the Rust core in carla_pack_registry._core."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
from pathlib import Path
from typing import Any, Callable

from . import _core
from ._core import CarlaPackError, Client
from .deploy import deploy_pack
from .publish import publish


class CliError(Exception):
    """User-facing error with a message only."""


# --------------------------------------------------------------------------- helpers


def _client(args: argparse.Namespace) -> Client:
    return Client(repos=args.repo_list or None)


def _out(args: argparse.Namespace, data: Any, human: Callable[[], None]) -> None:
    if args.json:
        print(json.dumps(data, indent=2, ensure_ascii=False, default=str))
    else:
        human()


def _table(rows: list[list[str]], header: list[str], empty: str) -> None:
    if not rows:
        print(empty)
        return
    widths = [max(len(str(r[i])) for r in [header, *rows]) for i in range(len(header))]
    for r in [header, *rows]:
        print("  ".join(str(c).ljust(w) for c, w in zip(r, widths)).rstrip())


def _size(n: int) -> str:
    size = float(n)
    for unit in ("B", "KiB", "MiB"):
        if size < 1024:
            return f"{n} B" if unit == "B" else f"{size:.1f} {unit}"
        size /= 1024
    return f"{size:.1f} GiB"


def _filters(args: argparse.Namespace) -> dict[str, Any]:
    return {"base_release": args.base_release, "platform": args.platform, "sha256": args.sha256}


def _release(client: Client, args: argparse.Namespace) -> dict[str, Any]:
    """`target` is OWNER/REPO/PACK[@VERSION], or a CARLA map name with --map."""
    if args.map:
        return client.find_map(args.target, **_filters(args))
    return client.resolve(args.target, **_filters(args))


def _carla_client(address: str) -> Any:
    try:
        import typesafe_carla.carla as carla  # type: ignore[import-not-found]
    except ImportError:
        try:
            import carla  # type: ignore[import-not-found,no-redef]
        except ImportError:
            raise CliError("--mount needs the CARLA Python API (typesafe-carla or your package's carla wheel)") from None
    host, sep, port = address.rpartition(":")
    if not sep:
        host, port = address, "2000"
    c = carla.Client(host, int(port))
    c.set_timeout(60.0)
    return c


# --------------------------------------------------------------------------- auth


def cmd_login(args: argparse.Namespace) -> int:
    if args.with_token:
        token = sys.stdin.readline().strip()
        if not token:
            raise CliError("no token on standard input")
    else:
        client_id = args.client_id or os.environ.get("CARLA_PACK_GITHUB_CLIENT_ID")
        if not client_id:
            raise CliError(
                "device login needs the client ID of a GitHub App or OAuth App (--client-id or "
                "CARLA_PACK_GITHUB_CLIENT_ID); otherwise use `gh auth login` (carla-pack uses its token) or "
                "`carla-pack login --with-token < token.txt`"
            )
        token = _core.device_login(
            client_id,
            args.scope,
            lambda uri, code: print(f"Open {uri} and enter the code: {code}", flush=True),
        )
    user = Client(token=token).whoami()
    _core.save_login(token, user["login"])
    print(f"Logged in to GitHub as {user['login']}")
    return 0


def cmd_logout(args: argparse.Namespace) -> int:
    print("Removed the saved token" if _core.remove_login() else "No saved token")
    return 0


def cmd_whoami(args: argparse.Namespace) -> int:
    client = _client(args)
    if client.token_source is None:
        raise CliError("no GitHub token: run `carla-pack login`, `gh auth login`, or set GH_TOKEN")
    user = client.whoami()
    data = {"login": user["login"], "id": user["id"], "token_source": client.token_source, "repos": client.repos}
    _out(args, data, lambda: print(f"{user['login']} (GitHub id {user['id']}, token from {client.token_source})"))
    return 0


# --------------------------------------------------------------------------- repositories


def cmd_repo_add(args: argparse.Namespace) -> int:
    for repo in args.repos:
        if args.check:
            Client(repos=[]).list_packs(repo)  # fails unless the manifest is readable
        print(f"added {repo}" if _core.add_repo(repo) else f"{repo} is already listed")
    return 0


def cmd_repo_rm(args: argparse.Namespace) -> int:
    for repo in args.repos:
        print(f"removed {repo}" if _core.remove_repo(repo) else f"{repo} was not listed")
    return 0


def cmd_repo_ls(args: argparse.Namespace) -> int:
    repos = Client().repos
    env = bool(os.environ.get("CARLA_PACK_REPOS", "").strip())
    _out(args, repos, lambda: [print(r) for r in repos] if repos else print("(none: carla-pack repo add OWNER/REPO)"))
    if env and not args.json:
        print("(from CARLA_PACK_REPOS)", file=sys.stderr)
    return 0


# --------------------------------------------------------------------------- packs


def cmd_ls(args: argparse.Namespace) -> int:
    client = _client(args)
    repos = [args.repo] if args.repo else client.repos
    if not repos:
        raise CliError("no pack repositories: pass OWNER/REPO, run `carla-pack repo add`, or set CARLA_PACK_REPOS")
    releases: list[dict[str, Any]] = []
    errors: list[str] = []
    for repo in repos:
        try:
            releases.extend(client.list_packs(repo))
        except CarlaPackError as e:
            if args.repo:
                raise
            errors.append(f"{repo}: {e.args[0]}")

    def human() -> None:
        _table(
            [
                [
                    r["ref"],
                    r["base_release"],
                    r["platform"],
                    _size(r["size"]),
                    ", ".join(r["maps"]) + (" (yanked)" if r.get("yanked") else ""),
                ]
                for r in releases
            ],
            ["PACK", "BASE RELEASE", "PLATFORM", "SIZE", "MAPS"],
            "(no packs)",
        )
        for e in errors:
            print(f"warning: {e}", file=sys.stderr)

    _out(args, releases, human)
    return 0


def cmd_find(args: argparse.Namespace) -> int:
    r = _client(args).find_map(args.map_name, **_filters(args))
    _out(args, r, lambda: print(f"{r['ref']}  ({r['base_release']}, {r['platform']}, {_size(r['size'])})"))
    return 0


def cmd_pull(args: argparse.Namespace) -> int:
    client = _client(args)
    release = _release(client, args)
    cached = client.fetch(release)
    out_dir = Path(args.output)
    out_dir.mkdir(parents=True, exist_ok=True)
    dest = out_dir / Path(release["key"]).name
    shutil.copyfile(cached, dest)
    data = {**release, "path": str(dest)}
    _out(args, data, lambda: print(f"{release['ref']} -> {dest} ({_size(release['size'])}, sha256 verified)"))
    return 0


def cmd_install(args: argparse.Namespace) -> int:
    client = _client(args)
    release = _release(client, args)
    pack = deploy_pack(
        release,
        args.packs_dir,
        carla_client=_carla_client(args.mount) if args.mount else None,
        base_release=args.base_release,
        force=args.force,
        client=client,
    )

    def human() -> None:
        verb = "already installed" if pack["already_installed"] else "installed"
        print(f"{verb}: {release['ref']} ({pack['base_release']}) -> {pack['path']}")
        print(f"  maps: {', '.join(pack['maps']) or '-'}")
        if pack["mounted"]:
            print(f"  mounted on {args.mount}")
        else:
            print(f"  not mounted: use --mount HOST[:PORT] or client.mount_content_pack({str(pack['path'])!r})")

    _out(args, {**pack, "ref": release["ref"], "sha256": release["sha256"]}, human)
    return 0


def cmd_credentials(args: argparse.Namespace) -> int:
    s = _client(args).credentials_status(args.repo)

    def human() -> None:
        print(f"{s['repo']}: {s['storage']} ({s['mode']})")
        if s.get("access_key_id"):
            print(f"  access key : {s['access_key_id']}")
            print(f"  expires at : {s.get('expires_at') or 'never (long-lived key)'}")

    _out(args, s, human)
    return 0


def cmd_publish(args: argparse.Namespace) -> int:
    entry = publish(
        args.manifest,
        args.tarball,
        key=args.key,
        upload=not args.no_upload,
        overwrite=args.overwrite,
        description=args.description,
        part_size=args.part_size_mib * 1024 * 1024,
    )

    def human() -> None:
        done = "added" if args.no_upload else "uploaded and added"
        print(f"{done}: {entry['name']} {entry['version']} ({entry['base_release']}, {entry['platform']})")
        print(f"  key    : {entry['key']}")
        print(f"  sha256 : {entry['sha256']}")
        print(f"  maps   : {', '.join(entry['maps']) or '-'}")
        print(f"\nCommit and push {args.manifest} to publish it.")

    _out(args, entry, human)
    return 0


# --------------------------------------------------------------------------- parser


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="carla-pack",
        description="Download CARLA content packs from GitHub-gated S3-compatible storage.",
    )
    p.add_argument("--version", action="version", version=f"carla-pack {_core.__version__}")
    p.add_argument("--json", action="store_true", help="machine-readable output")
    p.add_argument(
        "--repo",
        dest="repo_list",
        action="append",
        metavar="OWNER/REPO",
        help="search this pack repository (repeatable; default: CARLA_PACK_REPOS or `carla-pack repo add`)",
    )
    # The same options after the subcommand; SUPPRESS keeps a value given before it.
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--json", action="store_true", default=argparse.SUPPRESS, help="machine-readable output")
    common.add_argument(
        "--repo", dest="repo_list", action="append", metavar="OWNER/REPO", default=argparse.SUPPRESS,
        help="search this pack repository (repeatable)",
    )
    sub = p.add_subparsers(dest="command", required=True)

    def add(name: str, help: str, func: Callable[[argparse.Namespace], int], parent: Any = sub) -> Any:
        s = parent.add_parser(name, help=help, description=help, parents=[common])
        s.set_defaults(func=func)
        return s

    def selection(s: Any, map_target: bool = True) -> None:
        if map_target:
            s.add_argument("target", help="OWNER/REPO/PACK[@VERSION], or a CARLA map name with --map")
            s.add_argument("--map", action="store_true", help="TARGET is a map name: use the newest pack providing it")
        s.add_argument("--base-release", help="only packs built for this CARLA base release (e.g. carla-0.10.0-Linux)")
        s.add_argument("--platform", help="only packs for this platform (e.g. Linux)")
        s.add_argument("--sha256", help="only this exact artifact (as pinned in a lock file)")

    s = add("login", "save a GitHub token (device login, or --with-token from stdin)", cmd_login)
    s.add_argument("--with-token", action="store_true", help="read a token (e.g. a fine-grained PAT) from stdin")
    s.add_argument("--client-id", help="GitHub App / OAuth App client ID (default CARLA_PACK_GITHUB_CLIENT_ID)")
    s.add_argument("--scope", default="", help='OAuth App scope (GitHub Apps ignore it; an OAuth App needs "repo")')
    add("logout", "forget the saved token", cmd_logout)
    add("whoami", "show the GitHub user and where the token comes from", cmd_whoami)

    repo = add("repo", "pack repositories searched by map name", lambda a: (repo.print_help(), 0)[1])
    rsub = repo.add_subparsers(dest="repo_command")
    s = add("add", "add pack repositories", cmd_repo_add, rsub)
    s.add_argument("repos", nargs="+", metavar="OWNER/REPO")
    s.add_argument("--no-check", dest="check", action="store_false", help="add without reading its manifest")
    s = add("rm", "remove pack repositories", cmd_repo_rm, rsub)
    s.add_argument("repos", nargs="+", metavar="OWNER/REPO")
    add("ls", "list the pack repositories", cmd_repo_ls, rsub)

    s = add("ls", "list the packs of a repository (default: every listed repository)", cmd_ls)
    s.add_argument("repo", nargs="?", metavar="OWNER/REPO")
    s = add("find", "which pack provides a CARLA map", cmd_find)
    s.add_argument("map_name")
    selection(s, map_target=False)
    s = add("pull", "download a pack tarball (SHA-256 verified)", cmd_pull)
    selection(s)
    s.add_argument("-o", "--output", default=".", help="directory to write the tarball to")
    s = add("install", "download, verify and install a pack (optionally mount it)", cmd_install)
    selection(s)
    s.add_argument("--packs-dir", required=True, help="the CARLA server's packs folder (readable by the server)")
    s.add_argument("--mount", metavar="HOST[:PORT]", help="mount it on this running CARLA server")
    s.add_argument("-f", "--force", action="store_true", help="replace another installed version (unmounts it)")
    s = add("credentials", "show the storage credentials a repository hands out (no secrets)", cmd_credentials)
    s.add_argument("repo", metavar="OWNER/REPO")

    s = add("publish", "upload a pack and add it to a pack repository's carla-packs.toml", cmd_publish)
    s.add_argument("tarball", help="a pack built by CARLA's Util/ContentPacks/carla_pack.py")
    s.add_argument("--manifest", default="carla-packs.toml", help="carla-packs.toml in a checkout of the pack repository")
    s.add_argument("--key", help="object key below storage.prefix (default: the tarball's file name)")
    s.add_argument("--no-upload", action="store_true", help="only add the entry (you uploaded the file yourself)")
    s.add_argument("--overwrite", action="store_true", help="replace an object already stored under that key")
    s.add_argument("--description")
    s.add_argument(
        "--part-size-mib", type=int, default=64, metavar="MIB",
        help="multipart upload part size (default 64; files up to one part go up in a single PUT)",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.func(args) or 0)
    except CliError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except CarlaPackError as e:
        print(f"error: {e.args[0]}", file=sys.stderr)
        return 130 if e.args[1] == "interrupted" else 1
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        return 130
