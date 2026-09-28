#!/usr/bin/env python3
"""Run combined checks and verify isolated packages and external consumers."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
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


CHECKS_PASSED = 0
CHECKS_WITH_WARNINGS = 0


def status(text, color):
    if "NO_COLOR" in os.environ or not sys.stdout.isatty():
        return text
    codes = {"green": "32", "yellow": "33", "red": "31", "cyan": "36"}
    return f"\033[{codes[color]}m{text}\033[0m"


def run(*command, cwd, env=None):
    global CHECKS_PASSED, CHECKS_WITH_WARNINGS
    print(status("▶", "cyan"), " ".join(map(str, command)), flush=True)
    process = subprocess.Popen(
        command, cwd=cwd, env=env, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, bufsize=1)
    assert process.stdout is not None
    command_warnings = 0
    for line in process.stdout:
        print(line, end="", flush=True)
        if "warning:" in line.lower():
            command_warnings += 1
    return_code = process.wait()
    if return_code:
        print(status(f"✗ CHECK FAILED (exit={return_code})", "red"), flush=True)
        raise subprocess.CalledProcessError(return_code, command)
    CHECKS_PASSED += 1
    if command_warnings:
        CHECKS_WITH_WARNINGS += command_warnings
        print(status(f"⚠ CHECK PASSED WITH {command_warnings} WARNING(S)", "yellow"), flush=True)
    else:
        print(status("✓ CHECK PASSED", "green"), flush=True)


def check(path):
    run("cargo", "fmt", "--all", "--", "--check", cwd=path)
    run("cargo", "clippy", "--offline", "--locked", "--workspace", "--all-targets",
        "--all-features", "--", "-D", "warnings", cwd=path)
    run("cargo", "test", "--offline", "--locked", "--workspace", "--all-features", cwd=path)
    run("cargo", "test", "--offline", "--locked", "--workspace", "--no-default-features", cwd=path)
    run("cargo", "doc", "--offline", "--locked", "--workspace", "--no-deps", "--all-features",
        cwd=path, env={**os.environ, "RUSTDOCFLAGS": "-D warnings"})


def check_examples(root):
    examples = (
        ("01-capability", "rustclamp-example-capability"),
        ("02-module", "rustclamp-example-module"),
        ("03-contribution", "rustclamp-example-contribution"),
        ("04-process", "rustclamp-example-process"),
        ("05-lifecycle", "rustclamp-example-lifecycle"),
    )
    for example, package_name in examples:
        example_root = root / "rustclamp/examples" / example
        manifest = example_root / "Cargo.toml"
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
             "--manifest-path", str(manifest)], cwd=root, text=True))
        packages = {package["id"]: package["name"] for package in metadata["packages"]}
        nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
        package_id = next(key for key, name in packages.items() if name == package_name)
        pending = [dependency["pkg"] for dependency in nodes[package_id]["deps"]]
        reached = set()
        while pending:
            dependency = pending.pop()
            if dependency not in reached:
                reached.add(dependency)
                pending.extend(item["pkg"] for item in nodes[dependency]["deps"])
        dependencies = {packages[item] for item in reached}
        assert "rustclamp" not in dependencies, f"{example} unexpectedly depends on the facade"
        print(f"{example} dependency graph: {', '.join(sorted(dependencies))}; facade absent")

        run("cargo", "fmt", "--manifest-path", str(manifest), "--", "--check", cwd=root)
        run("cargo", "clippy", "--offline", "--locked", "--manifest-path", str(manifest),
            "--all-targets", "--", "-D", "warnings", cwd=root)
        run("cargo", "test", "--offline", "--locked", "--manifest-path", str(manifest), cwd=root)
        run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
            "--example", example, cwd=root)
        if example == "03-contribution":
                run("cargo", "bench", "--offline", "--locked", "--manifest-path", str(manifest),
                    "--bench", "assembly", cwd=root)


def check_process_build_targets(root):
    manifest = root / "rustclamp/examples/04-process/build-targets/Cargo.toml"
    modes = (
        ("runtime-selected", "runtime-selected", [["cli"], ["worker"]],
         {"rustclamp-core", "rustclamp-kernel"}),
        ("cli-target", "cli-target", [[]], {"rustclamp-core"}),
        ("worker-target", "worker-target", [[]],
         {"rustclamp-core", "rustclamp-kernel"}),
    )
    run("cargo", "fmt", "--manifest-path", str(manifest), "--", "--check", cwd=root)
    for feature, binary, runs, expected_dependencies in modes:
        mode = ["--no-default-features", "--features", feature, "--bin", binary]
        run("cargo", "clippy", "--offline", "--locked", "--manifest-path", str(manifest),
            *mode, "--", "-D", "warnings", cwd=root)
        run("cargo", "test", "--offline", "--locked", "--manifest-path", str(manifest),
            *mode, cwd=root)
        for arguments in runs:
            run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
                *mode, "--", *arguments, cwd=root)
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
             "--manifest-path", str(manifest), "--no-default-features", "--features", feature],
            cwd=root, text=True))
        package = next(item for item in metadata["packages"]
                       if item["name"] == "rustclamp-process-build-targets")
        nodes = {item["id"]: item for item in metadata["resolve"]["nodes"]}
        pending = [edge["pkg"] for edge in nodes[package["id"]]["deps"]]
        reached = set()
        while pending:
            current = pending.pop()
            if current not in reached:
                reached.add(current)
                pending.extend(edge["pkg"] for edge in nodes[current]["deps"])
        names = sorted({item["name"] for item in metadata["packages"]
                        if item["id"] in reached})
        assert set(names) == expected_dependencies, (
            f"{feature} dependency closure changed: expected "
            f"{sorted(expected_dependencies)}, found {names}"
        )
        print(f"{feature} build dependency graph: {', '.join(names) or '(none)'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        run("python3", str(root / "rustclamp/tools/workspace.py"), "--root", str(root), cwd=root)
        run("cargo", "generate-lockfile", "--offline", cwd=root)
        run("python3", str(root / "rustclamp/tools/boundaries.py"), "--manifest",
            str(root / "Cargo.toml"), "--expect-four", cwd=root)
        check(root)
        check_examples(root)
        check_process_build_targets(root)
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
    except Exception:
        print(status(f"✗ FULL CHECK FAILED after {CHECKS_PASSED} successful commands", "red"), flush=True)
        raise
    if CHECKS_WITH_WARNINGS:
        print(status(
            f"⚠ FULL CHECK COMPLETED WITH WARNINGS ({CHECKS_PASSED} commands, {CHECKS_WITH_WARNINGS} warnings)",
            "yellow"), flush=True)
    else:
        print(status(f"✓ FULL CHECK SUCCESSFULLY COMPLETED ({CHECKS_PASSED} commands)", "green"), flush=True)


if __name__ == "__main__":
    main()
