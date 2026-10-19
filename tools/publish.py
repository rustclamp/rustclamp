#!/usr/bin/env python3
"""Publish every RustClamp crate to crates.io in dependency order.

Without --execute this is a dry run: each crate is packaged and built with its
unpublished siblings patched in from the sibling checkouts, so the whole set is
checked before anything is uploaded. With --execute the crates are uploaded in
order, unpatched; cargo waits for each one to reach the index before the next.
"""

import argparse
from pathlib import Path
import subprocess
import sys

# Dependency order: a crate comes after everything it depends on.
CRATES = (
    ("rustclamp-core", "core"),
    ("rustclamp-macros", "rustclamp/macros"),
    ("rustclamp-kernel", "kernel"),
    ("rustclamp-runtime", "runtime"),
    ("rustclamp-messaging", "messaging"),
    ("rustclamp-http", "http"),
    ("rustclamp-postgres", "postgres"),
    ("rustclamp-scheduler", "scheduler"),
    ("rustclamp-worker", "worker"),
    ("rustclamp", "rustclamp"),
    ("rustclamp-tooling", "rustclamp/tooling"),
)


def patches(root):
    """`--config` arguments pointing every crate at its sibling checkout."""
    arguments = []
    for name, directory in CRATES:
        arguments += ["--config", f'patch.crates-io.{name}.path="{root / directory}"']
    # The facade names core by git (#86); a dry run uses the checkout instead.
    arguments += ["--config", f'patch."https://github.com/rustclamp/core".rustclamp-core.path="{root / "core"}"']
    return arguments


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--execute", action="store_true", help="upload instead of a dry run")
    parser.add_argument("--from", dest="start", help="resume at this crate after a failed upload")
    args = parser.parse_args()
    root = args.root.resolve()
    names = [name for name, _ in CRATES]
    if args.start and args.start not in names:
        parser.error(f"unknown crate {args.start}; one of {', '.join(names)}")
    crates = CRATES[names.index(args.start):] if args.start else CRATES
    for name, directory in crates:
        if not (root / directory / "Cargo.toml").is_file():
            parser.error(f"missing checkout: {root / directory}")
        command = ["cargo", "publish", "--manifest-path", str(root / directory / "Cargo.toml")]
        command += [] if args.execute else ["--dry-run", "--allow-dirty", *patches(root)]
        print(f"{'publishing' if args.execute else 'checking'} {name}", flush=True)
        # The patches leave `[[patch.unused]]` entries in the lockfile; a dry run keeps it as it was.
        lockfile = root / directory / "Cargo.lock"
        locked = lockfile.read_bytes() if lockfile.is_file() else None
        status = subprocess.run(command, cwd=root / directory).returncode
        if not args.execute and locked is not None:
            lockfile.write_bytes(locked)
        if status != 0:
            sys.exit(f"{name} failed; fix it, then rerun with --from {name}")
    print(f"{len(crates)} crates {'published' if args.execute else 'ready to publish'}")


if __name__ == "__main__":
    main()
