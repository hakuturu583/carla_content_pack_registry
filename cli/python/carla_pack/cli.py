"""`carla-pack` command line interface: argument parsing, prompts and output only.
The work (HTTP, login, credential storage, ACL edits, downloads) is done by the Rust
core in carla_pack._core."""

from __future__ import annotations

import argparse
import getpass
import json
import os
import sys
from typing import Any, Callable

from . import _core
from ._core import CarlaPackError, Client


class CliError(Exception):
    """User-facing error with a message only."""


# --------------------------------------------------------------------------- helpers


def _client(args: argparse.Namespace) -> Client:
    return Client(args.registry)


def _resolve(args: argparse.Namespace) -> tuple[Client, dict[str, Any]]:
    client = _client(args)
    return client, client.resolve_bucket(args.bucket)


def _out(args: argparse.Namespace, data: Any, human: Callable[[], None]) -> None:
    if args.json:
        print(json.dumps(data, indent=2, ensure_ascii=False))
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


def _print_bucket(b: dict[str, Any], next_step: str | None = None) -> None:
    print(f"{_core.bucket_ref(b)}  ({b['id']})")
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
    if next_step:
        print(f"\n{next_step}")


def _print_acl(entries: list[dict[str, Any]]) -> None:
    _table(
        [[e["type"], e["name"], str(e["id"])] for e in entries],
        ["TYPE", "NAME", "GITHUB ID"],
        "(no entries: only the owner can read this bucket unless it is public)",
    )


# --------------------------------------------------------------------------- auth


def cmd_login(args: argparse.Namespace) -> int:
    client = Client(args.registry, require_auth=False)
    token = args.token
    if not token:
        cfg = client.cli_config()
        if not cfg.get("githubClientId"):
            raise CliError(cfg.get("loginHint") or f"{client.registry_url} does not offer GitHub device login")
        token = _core.device_login(
            cfg["githubClientId"],
            cfg["oauthScopes"],
            lambda uri, code: print(f"Open {uri} and enter the code: {code}", flush=True),
        )
    me = client.with_token(token).me()
    _core.save_login(client.registry_url, token, me["user"]["login"])
    print(f"Logged in to {client.registry_url} as {me['user']['login']}")
    if me.get("membershipError"):
        print(f"warning: {me['membershipError']}", file=sys.stderr)
    return 0


def cmd_logout(args: argparse.Namespace) -> int:
    registry, removed = _core.remove_login(args.registry)
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
    if not args.bucket:
        buckets = _client(args).list_buckets()
        rows = [[_core.bucket_ref(b), b["visibility"], b["provider"], b["verifyStatus"]] for b in buckets]
        _out(args, buckets, lambda: _table(rows, ["BUCKET", "VISIBILITY", "PROVIDER", "STATUS"], "No buckets."))
        return 0
    client, bucket = _resolve(args)
    objects = client.list_objects(bucket["id"], args.prefix)
    rows = [[o["key"], _size(o["size"])] for o in objects]
    _out(args, objects, lambda: _table(rows, ["KEY", "SIZE"], "No objects."))
    return 0


def cmd_pull(args: argparse.Namespace) -> int:
    client, bucket = _resolve(args)

    def on_event(e: dict[str, Any]) -> None:
        if e["event"] == "skipped":
            print(f"skip   {e['key']} (exists; use --force to overwrite)", flush=True)
        else:
            print(f"pulled {e['key']} -> {e['dest']} ({_size(e['bytes'])})", flush=True)

    total = client.pull(bucket["id"], args.keys, args.output, force=args.force, jobs=args.jobs, on_event=on_event)
    print(f"done: {_size(total)} from {_core.bucket_ref(bucket)}")
    return 0


# --------------------------------------------------------------------------- bucket management


def cmd_bucket_add(args: argparse.Namespace) -> int:
    storage: dict[str, str] = {"provider": args.provider, "bucket": args.bucket_name}
    body: dict[str, Any] = {
        "name": args.name,
        "visibility": "public" if args.public else "private",
        "description": args.description or "",
        "storage": storage,
    }
    if args.provider == "s3":
        if not (args.region and args.role_arn):
            raise CliError("--region and --role-arn are required for S3")
        storage.update(region=args.region, roleArn=args.role_arn)
    else:
        if args.provider == "r2":
            if not args.account_id:
                raise CliError("--account-id is required for R2")
            storage["accountId"] = args.account_id
        body["credentials"] = _static_credentials(args)
    b = _client(args).create_bucket(body)
    next_step = f"Next: configure access, then run `carla-pack bucket verify {_core.bucket_ref(b)}`"
    _out(args, b, lambda: _print_bucket(b, next_step))
    return 0


def cmd_bucket_show(args: argparse.Namespace) -> int:
    _, b = _resolve(args)
    _out(args, b, lambda: _print_bucket(b))
    return 0


def cmd_bucket_set(args: argparse.Namespace) -> int:
    fields = (("name", args.name), ("description", args.description), ("visibility", args.visibility))
    patch = {k: v for k, v in fields if v is not None}
    if not patch:
        raise CliError("nothing to change (use --name, --description, --public or --private)")
    client, b = _resolve(args)
    updated = client.update_bucket(b["id"], patch)
    _out(args, updated, lambda: _print_bucket(updated))
    return 0


def cmd_bucket_credentials(args: argparse.Namespace) -> int:
    client, b = _resolve(args)
    if b["provider"] == "s3":
        body: dict[str, Any] = {
            "roleArn": args.role_arn or b.get("storage", {}).get("roleArn"),
            "rotateExternalId": args.rotate_external_id,
        }
    else:
        body = _static_credentials(args)
    updated = client.put_credentials(b["id"], body)
    _out(args, updated, lambda: _print_bucket(updated, f"Run `carla-pack bucket verify {_core.bucket_ref(updated)}`"))
    return 0


def cmd_bucket_verify(args: argparse.Namespace) -> int:
    client, b = _resolve(args)
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
    client, b = _resolve(args)
    ref = _core.bucket_ref(b)
    if not args.yes:
        if not sys.stdin.isatty():
            raise CliError("pass --yes to deregister without a prompt")
        if input(f"Deregister {ref}? Files in storage are NOT deleted. [y/N] ").strip().lower() not in ("y", "yes"):
            print("aborted")
            return 1
    client.delete_bucket(b["id"])
    print(f"deregistered {ref} (storage contents are untouched)")
    return 0


# --------------------------------------------------------------------------- ACL


def cmd_acl_show(args: argparse.Namespace) -> int:
    client, b = _resolve(args)
    entries = client.get_acl(b["id"])
    _out(args, entries, lambda: _print_acl(entries))
    return 0


def cmd_acl_change(args: argparse.Namespace) -> int:
    client, b = _resolve(args)
    entries = client.update_acl(b["id"], args.acl_command, args.user or [], args.org or [], args.team or [])
    _out(args, entries, lambda: _print_acl(entries))
    return 0


# --------------------------------------------------------------------------- parser


def _add_globals(p: argparse.ArgumentParser, *, top: bool) -> None:
    """--registry/--json work before or after the subcommand. Each parser gets its own
    option objects; on subcommands they default to SUPPRESS so they do not overwrite a
    value given before the subcommand."""
    p.add_argument(
        "--registry",
        default=None if top else argparse.SUPPRESS,
        help="registry URL (default: CARLA_PACK_REGISTRY or the last login)",
    )
    p.add_argument(
        "--json", action="store_true", default=False if top else argparse.SUPPRESS, help="machine-readable output"
    )


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="carla-pack", description="Discover, share and download CARLA Content Packs.")
    _add_globals(p, top=True)
    p.add_argument("--version", action="version", version=f"carla-pack {_core.__version__}")
    sub = p.add_subparsers(dest="command", required=True, metavar="COMMAND")

    def command(parent: Any, name: str, help: str, func: Callable[..., int] | None = None, *, bucket: bool = False):
        s = parent.add_parser(name, help=help)
        _add_globals(s, top=False)
        if bucket:
            s.add_argument("bucket", help="OWNER/NAME, NAME or bucket id")
        if func:
            s.set_defaults(func=func)
        return s

    s = command(sub, "login", "log in with GitHub (device flow)", cmd_login)
    s.add_argument("--token", help="use an existing GitHub token instead of the browser flow")
    command(sub, "logout", "forget the stored token", cmd_logout)
    command(sub, "whoami", "show the logged-in GitHub user", cmd_whoami)

    s = command(sub, "ls", "list buckets, or objects in a bucket", cmd_ls)
    s.add_argument("bucket", nargs="?", help="OWNER/NAME, NAME or bucket id")
    s.add_argument("prefix", nargs="?", help="only objects under this key prefix")

    s = command(sub, "pull", "download objects (directly from storage)", cmd_pull, bucket=True)
    s.add_argument("keys", nargs="*", help="object keys; a trailing / downloads a prefix (default: everything)")
    s.add_argument("-o", "--output", default=".", help="output directory (default: current directory)")
    s.add_argument("-f", "--force", action="store_true", help="overwrite existing files")
    s.add_argument("-j", "--jobs", type=int, default=4, help="parallel downloads (default: 4)")

    bsub = command(sub, "bucket", "register and manage your buckets").add_subparsers(
        dest="bucket_command", required=True, metavar="ACTION"
    )
    s = command(bsub, "add", "register a bucket you own", cmd_bucket_add)
    s.add_argument("name", help="display name in the registry")
    s.add_argument("--provider", choices=["r2", "s3", "local"], required=True)
    s.add_argument("--bucket", dest="bucket_name", required=True, help="bucket name in R2/S3")
    s.add_argument("--account-id", help="Cloudflare account id (R2)")
    s.add_argument("--region", help="AWS region (S3)")
    s.add_argument("--role-arn", help="IAM role the registry assumes (S3)")
    s.add_argument(
        "--access-key-id", help="bucket-scoped key id (R2/local); secret via CARLA_PACK_SECRET_ACCESS_KEY or prompt"
    )
    s.add_argument("--public", action="store_true", help="readable by every signed-in GitHub user (default: private)")
    s.add_argument("--description")
    command(bsub, "show", "show bucket details", cmd_bucket_show, bucket=True)
    s = command(bsub, "set", "change name, description or visibility", cmd_bucket_set, bucket=True)
    s.add_argument("--name")
    s.add_argument("--description")
    vis = s.add_mutually_exclusive_group()
    vis.add_argument("--public", dest="visibility", action="store_const", const="public")
    vis.add_argument("--private", dest="visibility", action="store_const", const="private")
    s = command(bsub, "credentials", "replace storage credentials (R2 keys / S3 role)", cmd_bucket_credentials, bucket=True)
    s.add_argument("--access-key-id")
    s.add_argument("--role-arn")
    s.add_argument("--rotate-external-id", action="store_true")
    command(bsub, "verify", "check storage access and that private storage is not public", cmd_bucket_verify, bucket=True)
    s = command(bsub, "rm", "deregister (storage contents are not deleted)", cmd_bucket_rm, bucket=True)
    s.add_argument("-y", "--yes", action="store_true")

    asub = command(sub, "acl", "who may read a private bucket").add_subparsers(
        dest="acl_command", required=True, metavar="ACTION"
    )
    command(asub, "show", "list entries", cmd_acl_show, bucket=True)
    for name, help_text in (("add", "grant read access"), ("rm", "revoke read access"), ("set", "replace all entries")):
        s = command(asub, name, help_text, cmd_acl_change, bucket=True)
        s.add_argument("--user", action="append", metavar="LOGIN")
        s.add_argument("--org", action="append", metavar="ORG")
        s.add_argument("--team", action="append", metavar="ORG/SLUG")

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
