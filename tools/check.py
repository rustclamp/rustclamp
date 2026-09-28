#!/usr/bin/env python3
"""Run combined checks and verify isolated packages and external consumers."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

from boundaries import ALLOWED
from workspace import REPOS

PACKAGE_TO_REPO = {
    "rustclamp": "rustclamp",
    "rustclamp-core": "core",
    "rustclamp-kernel": "kernel",
    "rustclamp-runtime": "runtime",
}


def local_dependency_repositories(root, repo):
    pending = [root / repo]
    visited = set()
    repositories = set()
    while pending:
        owner = pending.pop()
        package = tomllib.loads((owner / "Cargo.toml").read_text())
        sections = [package.get(section, {}) for section in (
            "dependencies", "dev-dependencies", "build-dependencies")]
        sections.extend(
            target.get(section, {})
            for target in package.get("target", {}).values()
            for section in ("dependencies", "dev-dependencies", "build-dependencies")
        )
        for section in sections:
            for alias, declaration in section.items():
                if not isinstance(declaration, dict) or "path" not in declaration:
                    continue
                name = declaration.get("package", alias)
                dependency_repo = PACKAGE_TO_REPO.get(name)
                if dependency_repo and dependency_repo not in visited:
                    visited.add(dependency_repo)
                    repositories.add(dependency_repo)
                    pending.append((owner / declaration["path"]).resolve())
    return repositories - {repo}


def allowed_package_closure(package):
    pending = [package]
    packages = set()
    while pending:
        current = pending.pop()
        for dependency in ALLOWED.get(current, set()):
            if dependency not in packages:
                packages.add(dependency)
                pending.append(dependency)
    return packages


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
    # Copy each package and only its declared internal dependency closure out of the
    # coordination workspace. Unrelated siblings cannot mask a package failure.
    for repo in REPOS:
        with tempfile.TemporaryDirectory(prefix=f"clamp-isolated-{repo}-") as tmp:
            isolated_root = Path(tmp) / "isolated"
            isolated_root.mkdir()
            isolated = isolated_root / repo
            shutil.copytree(root / repo, isolated, ignore=shutil.ignore_patterns(".git", "target", "__pycache__"))
            local_dependencies = local_dependency_repositories(root, repo)
            for dependency_repo in sorted(local_dependencies):
                shutil.copytree(
                    root / dependency_repo,
                    isolated_root / dependency_repo,
                    ignore=shutil.ignore_patterns(".git", "target", "__pycache__"),
                )
            check(isolated)
            if local_dependencies:
                # Unpublished internal path dependencies cannot be resolved from
                # the registry during cargo package's archive verification.
                run("cargo", "package", "--list", "--offline", "--locked", "--allow-dirty", cwd=isolated)
            else:
                run("cargo", "package", "--offline", "--locked", "--allow-dirty", cwd=isolated)
            consumer = Path(tmp) / "consumer"
            (consumer / "src").mkdir(parents=True)
            name = "rustclamp" if repo == "rustclamp" else f"rustclamp-{repo}"
            (consumer / "Cargo.toml").write_text(
                '[package]\nname="external-consumer"\nversion="0.0.0"\nedition="2024"\n'
                f'[dependencies]\n{name}={{version="0.1", path="../isolated/{repo}"}}\n')
            (consumer / "src/main.rs").write_text("fn main() {}\n")
            run("cargo", "+1.96.1", "check", "--offline", cwd=consumer)
            data = json.loads(subprocess.check_output(
                ["cargo", "+1.96.1", "metadata", "--offline", "--format-version", "1"],
                cwd=consumer, text=True))
            actual = {p["name"] for p in data["packages"]}
            permitted = allowed_package_closure(name) | {name, "external-consumer"}
            assert actual <= permitted, f"unexpected isolated dependency graph: {sorted(actual - permitted)}"
    print("Combined, isolated, packaging and relative-path consumer checks passed.")


if __name__ == "__main__":
    main()
