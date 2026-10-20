"""`carla-pack` command line interface. Argument parsing and output only; the work is
done by the Rust core (carla_pack._core)."""

from __future__ import annotations

import argparse
import getpass
import json
import os
import sys
import time
from pathlib import Path, PurePosixPath
from typing import Any, Callable

from . import _core
from ._core import CarlaPackError, Client

GITHUB_URL = os.environ.get("CARLA_PACK_GITHUB_URL", "https://github.com")


class CliError(Exception):
    """User-facing error with a message only."""


# --------------------------------------------------------------------------- helpers


def _client(args: argparse.Namespace, *, auth: bool = True) -> Client:
    registry = _core.resolve_registry(args.registry)
    token = _core.stored_token(registry)
    if auth and not token:
        raise CliError(f"not logged in to {registry}; run `carla-pack login`")
    return Client(registry, token)


def _out(args: argparse.Namespace, data: Any, human: Callable[[], None]) -> None:
    if args.json:
        print(json.dumps(data, indent=2, ensure_ascii=False))
    else:
        human()


def _table(rows: list[list[str]], header: list[str]) -> None:
    if not rows:
        return
    widths = [max(len(str(r[i])) for r in [header, *rows]) for i in range(len(header))]
    for r in [header, *rows]:
        print("  ".join(str(c).ljust(w) for c, w in zip(r, widths)).rstrip())


def _size(n: int) -> str:
    size = float(n)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if size < 1024 or unit == "GiB":
            return f"{size:.0f} {unit}" if unit == "B" else f"{size:.1f} {unit}"
        size /= 1024
    return f"{n} B"


def _ref(b: dict[str, Any]) -> str:
    return f"{b['owner'].get('login') or b['owner']['id']}/{b['name']}"


def _secret(env: str, prompt: str) -> str:
    """Secrets come from the environment or an interactive prompt, never from argv."""
    value = os.environ.get(env)
    if value:
        return value
    if not sys.stdin.isatty():
        raise CliError(f"set {env} (secrets are not accepted as command line arguments)")
    return getpass.getpass(prompt)


def _static_credentials(args: argparse.Namespace) -> dict[str, str]:
    key_id = args.access_key_id or os.environ.get("CARLA_PACK_ACCESS_KEY_ID")
    if not key_id:
        raise CliError("--access-key-id (or CARLA_PACK_ACCESS_KEY_ID) is required")
    return {
        "accessKeyId": key_id,
        "secretAccessKey": _secret("CARLA_PACK_SECRET_ACCESS_KEY", "Secret access key: "),
    }


def _print_bucket(b: dict[str, Any]) -> None:
    print(f"{_ref(b)}  ({b['id']})")
    print(f"  visibility : {b['visibility']}")
    print(f"  provider   : {b['provider']}")
    status = b["verifyStatus"] + (f" ({b['verifyDetail']})" if b.get("verifyDetail") else "")
    print(f"  status     : {status}")
    if b.get("description"):
        print(f"  description: {b['description']}")
    for k, v in (b.get("storage") or {}).items():
        if k != "provider":
            print(f"  {k:<11}: {v}")
    trust = b.get("awsTrust")
    if trust:
        print("  AWS role trust policy must allow:")
        print(f"    principal  : {trust.get('principalArn') or '(ask the registry operator)'}")
        print(f"    externalId : {trust['externalId']}")


# --------------------------------------------------------------------------- auth


def cmd_login(args: argparse.Namespace) -> int:
    registry = _core.resolve_registry(args.registry)
    client = Client(registry)
    token = args.token or (os.environ.get("CARLA_PACK_TOKEN") if args.token_from_env else None)
    if not token:
        cfg = client.cli_config()
        if not cfg.get("githubClientId"):
            raise CliError(
                f"{registry} does not offer GitHub device login (authMode={cfg.get('authMode')}). "
                "For local development use: carla-pack login --token mock:alice"
            )
        token = _device_login(cfg["githubClientId"], cfg.get("oauthScopes", "read:org"))
    me = Client(registry, token).me()
    _core.save_login(registry, token, me["user"]["login"])
    print(f"Logged in to {registry} as {me['user']['login']}")
    if me.get("membershipError"):
        print(f"warning: {me['membershipError']}", file=sys.stderr)
    return 0


def _device_login(client_id: str, scope: str) -> str:
    flow = _core.DeviceFlow(GITHUB_URL, client_id)
    code = flow.start(scope)
    print(f"Open {code['verification_uri']} and enter the code: {code['user_code']}")
    interval = int(code["interval"])
    deadline = time.monotonic() + int(code["expires_in"])
    while time.monotonic() < deadline:
        time.sleep(interval)
        state, token, new_interval = flow.poll(code["device_code"])
        if state == "token" and token:
            return token
        if state == "slow_down" and new_interval:
            interval = new_interval
    raise CliError("the login code expired; run `carla-pack login` again")


def cmd_logout(args: argparse.Namespace) -> int:
    registry = _core.resolve_registry(args.registry)
    removed = _core.remove_login(registry)
    print(f"Logged out of {registry}" if removed else f"Not logged in to {registry}")
    return 0


def cmd_whoami(args: argparse.Namespace) -> int:
    client = _client(args)
    me = client.me()

    def human() -> None:
        u = me["user"]
        print(f"{u['login']} (GitHub id {u['id']}) on {client.registry_url}")
        m = me.get("memberships")
        if m:
            print(f"  organizations: {len(m['orgIds'])}, teams: {len(m['teamIds'])}")
        if me.get("membershipError"):
            print(f"  warning: {me['membershipError']}")

    _out(args, me, human)
    return 0


# --------------------------------------------------------------------------- browse / download


def cmd_ls(args: argparse.Namespace) -> int:
    client = _client(args)
    if not args.bucket:
        buckets = client.list_buckets()

        def human_buckets() -> None:
            if not buckets:
                print("No buckets.")
            _table(
                [[_ref(b), b["visibility"], b["provider"], b["verifyStatus"]] for b in buckets],
                ["BUCKET", "VISIBILITY", "PROVIDER", "STATUS"],
            )

        _out(args, buckets, human_buckets)
        return 0

    bucket = client.resolve_bucket(args.bucket)
    objects = client.list_all_objects(bucket["id"], args.prefix)

    def human_objects() -> None:
        if not objects:
            print("No objects.")
        _table([[k, _size(s)] for k, s in objects], ["KEY", "SIZE"])

    _out(args, [{"key": k, "size": s} for k, s in objects], human_objects)
    return 0


def cmd_pull(args: argparse.Namespace) -> int:
    client = _client(args)
    bucket = client.resolve_bucket(args.bucket)
    out_dir = Path(args.output)
    # Exact keys, plus everything under arguments ending with "/" (or the whole bucket).
    keys: list[tuple[str, int | None]] = []
    targets = args.keys or [""]
    for t in targets:
        if t == "" or t.endswith("/"):
            keys.extend(client.list_all_objects(bucket["id"], t or None))
        else:
            keys.append((t, None))
    if not keys:
        raise CliError("nothing to download")
    total = 0
    for key, _ in keys:
        dest = _safe_dest(out_dir, key)
        if dest.exists() and not args.force:
            print(f"skip   {key} (exists; use --force to overwrite)")
            continue
        n = client.download(bucket["id"], key, dest)
        total += n
        print(f"pulled {key} -> {dest} ({_size(n)})")
    print(f"done: {_size(total)} from {_ref(bucket)}")
    return 0


def _safe_dest(out_dir: Path, key: str) -> Path:
    """Maps an object key below out_dir, refusing keys that would escape it."""
    parts = [p for p in PurePosixPath(key).parts if p not in ("", "/")]
    if not parts or any(p in (".", "..") for p in parts):
        raise CliError(f"refusing to write object key {key!r} outside the output directory")
    return out_dir.joinpath(*parts)


# --------------------------------------------------------------------------- bucket management


def cmd_bucket_add(args: argparse.Namespace) -> int:
    client = _client(args)
    storage: dict[str, str] = {"provider": args.provider, "bucket": args.bucket}
    body: dict[str, Any] = {
        "name": args.name,
        "visibility": "public" if args.public else "private",
        "description": args.description or "",
        "storage": storage,
    }
    if args.provider == "r2":
        if not args.account_id:
            raise CliError("--account-id is required for R2")
        storage["accountId"] = args.account_id
        body["credentials"] = _static_credentials(args)
    elif args.provider == "s3":
        if not (args.region and args.role_arn):
            raise CliError("--region and --role-arn are required for S3")
        storage.update(region=args.region, roleArn=args.role_arn)
    else:
        body["credentials"] = _static_credentials(args)
    b = client.create_bucket(body)

    def human() -> None:
        _print_bucket(b)
        print(f"\nNext: configure access, then run `carla-pack bucket verify {_ref(b)}`")

    _out(args, b, human)
    return 0


def cmd_bucket_show(args: argparse.Namespace) -> int:
    b = _client(args).resolve_bucket(args.bucket)
    _out(args, b, lambda: _print_bucket(b))
    return 0


def cmd_bucket_set(args: argparse.Namespace) -> int:
    client = _client(args)
    b = client.resolve_bucket(args.bucket)
    patch: dict[str, str] = {}
    if args.name:
        patch["name"] = args.name
    if args.description is not None:
        patch["description"] = args.description
    if args.visibility:
        patch["visibility"] = args.visibility
    if not patch:
        raise CliError("nothing to change (use --name, --description, --public or --private)")
    updated = client.update_bucket(b["id"], patch)
    _out(args, updated, lambda: _print_bucket(updated))
    return 0


def cmd_bucket_credentials(args: argparse.Namespace) -> int:
    client = _client(args)
    b = client.resolve_bucket(args.bucket)
    if b["provider"] == "s3":
        role = args.role_arn or b.get("storage", {}).get("roleArn")
        body: dict[str, Any] = {"roleArn": role, "rotateExternalId": args.rotate_external_id}
    else:
        body = _static_credentials(args)
    updated = client.put_credentials(b["id"], body)

    def human() -> None:
        _print_bucket(updated)
        print(f"\nRun `carla-pack bucket verify {_ref(updated)}`")

    _out(args, updated, human)
    return 0


def cmd_bucket_verify(args: argparse.Namespace) -> int:
    client = _client(args)
    b = client.resolve_bucket(args.bucket)
    result = client.verify_bucket(b["id"])
    report = result["report"]

    def human() -> None:
        for c in report["checks"]:
            print(f"{'ok  ' if c['ok'] else 'FAIL'}  {c['name']}" + (f": {c['detail']}" if c.get("detail") else ""))
        for w in report["warnings"]:
            print(f"warn  {w}")
        print("verified" if report["ok"] else "verification failed")

    _out(args, result, human)
    return 0 if report["ok"] else 1


def cmd_bucket_rm(args: argparse.Namespace) -> int:
    client = _client(args)
    b = client.resolve_bucket(args.bucket)
    if not args.yes:
        if not sys.stdin.isatty():
            raise CliError("pass --yes to deregister without a prompt")
        answer = input(f"Deregister {_ref(b)}? Files in storage are NOT deleted. [y/N] ")
        if answer.strip().lower() not in ("y", "yes"):
            print("aborted")
            return 1
    client.delete_bucket(b["id"])
    print(f"deregistered {_ref(b)} (storage contents are untouched)")
    return 0


# --------------------------------------------------------------------------- ACL


def _acl_refs(args: argparse.Namespace) -> list[dict[str, str]]:
    refs: list[dict[str, str]] = [{"type": "user", "login": u} for u in args.user or []]
    refs += [{"type": "org", "login": o} for o in args.org or []]
    for t in args.team or []:
        org, sep, slug = t.partition("/")
        if not sep or not org or not slug:
            raise CliError(f"--team must look like ORG/TEAM-SLUG, got {t!r}")
        refs.append({"type": "team", "org": org, "slug": slug})
    return refs


def _as_ref(e: dict[str, Any]) -> dict[str, str]:
    """Converts a stored entry back to the name form the API accepts."""
    if e["type"] == "team":
        org, _, slug = e["name"].partition("/")
        return {"type": "team", "org": org, "slug": slug}
    return {"type": e["type"], "login": e["name"]}


def _ref_key(r: dict[str, str]) -> str:
    return (f"team:{r['org']}/{r['slug']}" if r["type"] == "team" else f"{r['type']}:{r['login']}").lower()


def _print_acl(entries: list[dict[str, Any]]) -> None:
    if not entries:
        print("(no entries: only the owner can read this bucket unless it is public)")
    _table([[e["type"], e["name"], str(e["id"])] for e in entries], ["TYPE", "NAME", "GITHUB ID"])


def cmd_acl_show(args: argparse.Namespace) -> int:
    client = _client(args)
    entries = client.get_acl(client.resolve_bucket(args.bucket)["id"])
    _out(args, entries, lambda: _print_acl(entries))
    return 0


def cmd_acl_change(args: argparse.Namespace) -> int:
    client = _client(args)
    bucket_id = client.resolve_bucket(args.bucket)["id"]
    given = _acl_refs(args)
    if args.acl_command == "set":
        refs = given
    else:
        if not given:
            raise CliError("specify at least one --user, --org or --team")
        current = [_as_ref(e) for e in client.get_acl(bucket_id)]
        if args.acl_command == "add":
            refs = current + given
        else:
            drop = {_ref_key(r) for r in given}
            refs = [r for r in current if _ref_key(r) not in drop]
    entries = client.put_acl(bucket_id, refs)
    _out(args, entries, lambda: _print_acl(entries))
    return 0


# --------------------------------------------------------------------------- parser


def build_parser() -> argparse.ArgumentParser:
    # Global options are accepted before or after the subcommand. SUPPRESS keeps a
    # subcommand's (absent) option from overwriting one given before it.
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--registry", default=argparse.SUPPRESS, help="registry URL (default: CARLA_PACK_REGISTRY or the last login)")
    common.add_argument("--json", action="store_true", default=argparse.SUPPRESS, help="machine-readable output")

    p = argparse.ArgumentParser(
        prog="carla-pack",
        description="Discover, share and download CARLA Content Packs.",
        parents=[common],
    )
    p.add_argument("--version", action="version", version=f"carla-pack {_core.__version__}")
    sub = p.add_subparsers(dest="command", required=True, metavar="COMMAND")

    s = sub.add_parser("login", parents=[common], help="log in with GitHub (device flow)")
    s.add_argument("--token", help="use an existing GitHub token instead of the browser flow (local: mock:<user>)")
    s.add_argument("--token-from-env", action="store_true", help="read the token from CARLA_PACK_TOKEN")
    s.set_defaults(func=cmd_login)

    s = sub.add_parser("logout", parents=[common], help="forget the stored token")
    s.set_defaults(func=cmd_logout)

    s = sub.add_parser("whoami", parents=[common], help="show the logged-in GitHub user")
    s.set_defaults(func=cmd_whoami)

    s = sub.add_parser("ls", parents=[common], help="list buckets, or objects in a bucket")
    s.add_argument("bucket", nargs="?", help="OWNER/NAME, NAME or bucket id")
    s.add_argument("prefix", nargs="?", help="only objects under this key prefix")
    s.set_defaults(func=cmd_ls)

    s = sub.add_parser("pull", parents=[common], help="download objects (directly from storage)")
    s.add_argument("bucket", help="OWNER/NAME, NAME or bucket id")
    s.add_argument("keys", nargs="*", help="object keys; a trailing / downloads a prefix (default: everything)")
    s.add_argument("-o", "--output", default=".", help="output directory (default: current directory)")
    s.add_argument("-f", "--force", action="store_true", help="overwrite existing files")
    s.set_defaults(func=cmd_pull)

    b = sub.add_parser("bucket", parents=[common], help="register and manage your buckets")
    bsub = b.add_subparsers(dest="bucket_command", required=True, metavar="ACTION")

    s = bsub.add_parser("add", parents=[common], help="register a bucket you own")
    s.add_argument("name", help="display name in the registry")
    s.add_argument("--provider", choices=["r2", "s3", "local"], required=True)
    s.add_argument("--bucket", required=True, help="bucket name in R2/S3")
    s.add_argument("--account-id", help="Cloudflare account id (R2)")
    s.add_argument("--region", help="AWS region (S3)")
    s.add_argument("--role-arn", help="IAM role the registry assumes (S3)")
    s.add_argument("--access-key-id", help="bucket-scoped access key id (R2/local); secret via CARLA_PACK_SECRET_ACCESS_KEY or prompt")
    s.add_argument("--public", action="store_true", help="readable by every signed-in GitHub user (default: private)")
    s.add_argument("--description")
    s.set_defaults(func=cmd_bucket_add)

    s = bsub.add_parser("show", parents=[common], help="show bucket details")
    s.add_argument("bucket")
    s.set_defaults(func=cmd_bucket_show)

    s = bsub.add_parser("set", parents=[common], help="change name, description or visibility")
    s.add_argument("bucket")
    s.add_argument("--name")
    s.add_argument("--description")
    vis = s.add_mutually_exclusive_group()
    vis.add_argument("--public", dest="visibility", action="store_const", const="public")
    vis.add_argument("--private", dest="visibility", action="store_const", const="private")
    s.set_defaults(func=cmd_bucket_set)

    s = bsub.add_parser("credentials", parents=[common], help="replace storage credentials (R2 keys / S3 role)")
    s.add_argument("bucket")
    s.add_argument("--access-key-id")
    s.add_argument("--role-arn")
    s.add_argument("--rotate-external-id", action="store_true")
    s.set_defaults(func=cmd_bucket_credentials)

    s = bsub.add_parser("verify", parents=[common], help="check storage access and that private storage is not public")
    s.add_argument("bucket")
    s.set_defaults(func=cmd_bucket_verify)

    s = bsub.add_parser("rm", parents=[common], help="deregister (storage contents are not deleted)")
    s.add_argument("bucket")
    s.add_argument("-y", "--yes", action="store_true")
    s.set_defaults(func=cmd_bucket_rm)

    a = sub.add_parser("acl", parents=[common], help="who may read a private bucket")
    asub = a.add_subparsers(dest="acl_command", required=True, metavar="ACTION")
    s = asub.add_parser("show", parents=[common], help="list entries")
    s.add_argument("bucket")
    s.set_defaults(func=cmd_acl_show)
    for name, help_text in (("add", "grant read access"), ("rm", "revoke read access"), ("set", "replace all entries")):
        s = asub.add_parser(name, parents=[common], help=help_text)
        s.add_argument("bucket")
        s.add_argument("--user", action="append", metavar="LOGIN")
        s.add_argument("--org", action="append", metavar="ORG")
        s.add_argument("--team", action="append", metavar="ORG/SLUG")
        s.set_defaults(func=cmd_acl_change)

    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    # Defaults for the global options are filled in here, not with set_defaults():
    # argparse shares option objects between parsers built from the same parent.
    args.registry = getattr(args, "registry", None)
    args.json = getattr(args, "json", False)
    try:
        return int(args.func(args) or 0)
    except CliError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except CarlaPackError as e:
        message, code, _status = (list(e.args) + ["", "", 0])[:3]
        hint = "; run `carla-pack login`" if code == "unauthenticated" else ""
        print(f"error: {message}{hint}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        return 130
