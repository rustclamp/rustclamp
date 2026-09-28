#!/usr/bin/env python3
"""Run combined checks and verify isolated packages and external consumers."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from workspace import REPOS


def run(*command, cwd, env=None):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(command, cwd=cwd, env=env, check=True)


def check(path):
    run("cargo", "fmt", "--all", "--", "--check", cwd=path)
    run("cargo", "clippy", "--offline", "--locked", "--workspace", "--all-targets",
        "--all-features", "--", "-D", "warnings", cwd=path)
    run("cargo", "test", "--offline", "--locked", "--workspace", "--all-features", cwd=path)
    run("cargo", "test", "--offline", "--locked", "--workspace", "--no-default-features", cwd=path)
    run("cargo", "doc", "--offline", "--locked", "--workspace", "--no-deps", "--all-features",
        cwd=path, env={**os.environ, "RUSTDOCFLAGS": "-D warnings"})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    root = args.root.resolve()
    run("python3", str(root / "rustclamp/tools/workspace.py"), "--root", str(root), cwd=root)
    run("cargo", "generate-lockfile", "--offline", cwd=root)
    run("python3", str(root / "rustclamp/tools/boundaries.py"), "--manifest",
        str(root / "Cargo.toml"), "--expect-four", cwd=root)
    check(root)
    # Copy packages out of the coordination workspace. No parent manifest, sibling
    # checkout, absolute dependency path or workspace inheritance can mask a failure.
    for repo in REPOS:
        with tempfile.TemporaryDirectory(prefix=f"clamp-isolated-{repo}-") as tmp:
            isolated = Path(tmp) / repo
            shutil.copytree(root / repo, isolated, ignore=shutil.ignore_patterns(".git", "target", "__pycache__"))
            check(isolated)
            run("cargo", "package", "--offline", "--locked", "--allow-dirty", cwd=isolated)
            consumer = Path(tmp) / "consumer"
            (consumer / "src").mkdir(parents=True)
            name = "rustclamp" if repo == "rustclamp" else f"rustclamp-{repo}"
            (consumer / "Cargo.toml").write_text(
                '[package]\nname="external-consumer"\nversion="0.0.0"\nedition="2024"\n'
                f'[dependencies]\n{name}={{version="0.1", path="../{repo}"}}\n')
            (consumer / "src/main.rs").write_text("fn main() {}\n")
            run("cargo", "+1.96.1", "check", "--offline", cwd=consumer)
            data = json.loads(subprocess.check_output(
                ["cargo", "+1.96.1", "metadata", "--offline", "--format-version", "1"],
                cwd=consumer, text=True))
            assert {p["name"] for p in data["packages"]} == {name, "external-consumer"}
    print("Combined, isolated, packaging and relative-path consumer checks passed.")


if __name__ == "__main__":
    main()
