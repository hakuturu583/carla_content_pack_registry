#!/usr/bin/env python3
"""Bumps the version of the carla-pack-registry Python package (the CLI in cli/).

The version has one source, `[package] version` in cli/Cargo.toml: cli/pyproject.toml
declares it dynamic and maturin reads it from there. cli/Cargo.lock repeats it for the
crate itself and `cargo --locked` rejects a lock that disagrees, so both are rewritten
together. The release workflow calls this; editing them by hand is how they drift.

    scripts/bump-cli-version.py patch    # 0.1.0 -> 0.1.1
    scripts/bump-cli-version.py minor    # 0.1.0 -> 0.2.0
    scripts/bump-cli-version.py major    # 0.1.0 -> 1.0.0
    scripts/bump-cli-version.py 1.4.2    # set exactly
    scripts/bump-cli-version.py --show   # print the current version

The only line printed to stdout is the resulting version, so the workflow can capture it.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

CLI = Path(__file__).resolve().parent.parent / "cli"
CARGO_TOML = CLI / "Cargo.toml"
CARGO_LOCK = CLI / "Cargo.lock"
PACKAGE = re.compile(r'(?m)(^\[package\][^\[]*?^version = )"([^"]+)"')


def crate_name() -> str:
    match = re.search(r'(?m)^\[package\][^\[]*?^name = "([^"]+)"', CARGO_TOML.read_text())
    if match is None:
        raise SystemExit(f"no [package] name in {CARGO_TOML}")
    return match.group(1)


def current_version() -> str:
    match = PACKAGE.search(CARGO_TOML.read_text())
    if match is None:
        raise SystemExit(f"no [package] version in {CARGO_TOML}")
    return match.group(2)


def next_version(current: str, spec: str) -> str:
    if spec not in {"major", "minor", "patch"}:
        if not re.fullmatch(r"\d+\.\d+\.\d+", spec):
            raise SystemExit(f"expected major|minor|patch or an X.Y.Z version, got {spec!r}")
        return spec
    major, minor, patch = (int(part) for part in current.split("."))
    if spec == "major":
        return f"{major + 1}.0.0"
    if spec == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def rewrite(path: Path, pattern: str, version: str) -> None:
    text, count = re.subn(pattern, rf'\g<1>"{version}"', path.read_text(), count=1)
    if count != 1:
        raise SystemExit(f"failed to rewrite the version in {path}")
    path.write_text(text)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    if sys.argv[1] == "--show":
        print(current_version())
        return
    version = next_version(current_version(), sys.argv[1])
    rewrite(CARGO_TOML, PACKAGE.pattern, version)
    # Only the crate's own entry: a dependency that shares the version number stays put.
    rewrite(CARGO_LOCK, rf'(\[\[package\]\]\nname = "{re.escape(crate_name())}"\nversion = )"[^"]+"', version)
    print(version)


if __name__ == "__main__":
    main()
