#!/usr/bin/env python3
"""Enforce the initial dependency allowlist using resolved Cargo metadata."""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile

# Every new edge requires prototype evidence and an explicit policy change.
ALLOWED = {
    "rustclamp": {"rustclamp-core", "rustclamp-kernel", "rustclamp-runtime"},
    "rustclamp-core": set(),
    "rustclamp-kernel": {"rustclamp-core"},
    "rustclamp-runtime": {"rustclamp-core"},
}


def metadata(manifest):
    return json.loads(subprocess.check_output([
        "cargo", "metadata", "--format-version", "1", "--offline",
        "--all-features", "--manifest-path", str(manifest),
    ], text=True))


def violations(data):
    errors = []
    packages = {p["id"]: p for p in data["packages"]}
    for package in packages.values():
        name = package["name"]
        if name not in ALLOWED:
            continue
        # Declarations cover inactive target-specific and optional dependencies too.
        # Cargo reports the actual package name even for renamed dependencies.
        for dep in package["dependencies"]:
            if dep["name"] not in ALLOWED[name]:
                errors.append(f"{name} -> {dep['name']}: forbidden dependency")
            if dep.get("path") and dep["req"] == "*":
                errors.append(f"{name} -> {dep['name']}: path requires a version")
    # Check the complete resolved closure, not just immediate edges.
    nodes = {n["id"]: n for n in data["resolve"]["nodes"]}
    for package in packages.values():
        name = package["name"]
        if name not in ALLOWED:
            continue
        pending = list(nodes[package["id"]]["dependencies"])
        visited = set()
        while pending:
            dependency = pending.pop()
            if dependency in visited:
                continue
            visited.add(dependency)
            dep_name = packages[dependency]["name"]
            if dep_name not in ALLOWED[name]:
                errors.append(f"{name} reaches {dep_name}: forbidden transitive dependency")
            pending.extend(nodes[dependency]["dependencies"])
    return sorted(set(errors))


def self_test():
    # Real manifests, not hand-written metadata. These never access the registry.
    cases = [
        ("rustclamp-core", "tokio", "normal", False),
        ("rustclamp-core", "reqwest", "optional", False),
        ("rustclamp-core", "sqlx", "build", False),
        ("rustclamp-core", "async-openai", "target", False),
        ("rustclamp-kernel", "axum", "normal", False),
        ("rustclamp-runtime", "tokio", "normal", False),
        ("rustclamp-kernel", "rustclamp", "dev", False),
        ("rustclamp-kernel", "rustclamp-core", "normal", True),
    ]
    for owner, dependency, kind, valid in cases:
        with tempfile.TemporaryDirectory(prefix="clamp-boundary-") as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text('[workspace]\nresolver="3"\nmembers=["owner", "dependency"]\n')
            for dirname, name in [("owner", owner), ("dependency", dependency)]:
                path = root / dirname
                (path / "src").mkdir(parents=True)
                (path / "src/lib.rs").write_text("")
                (path / "Cargo.toml").write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2024"\n')
            section = {
                "normal": "dependencies", "optional": "dependencies",
                "build": "build-dependencies", "dev": "dev-dependencies",
                "target": "target.'cfg(target_os = \"none\")'.dependencies",
            }[kind]
            optional = ", optional=true" if kind == "optional" else ""
            with (root / "owner/Cargo.toml").open("a") as file:
                file.write(f'\n[{section}]\nhidden_alias={{package="{dependency}", version="0.1", path="../dependency"{optional}}}\n')
            errors = violations(metadata(root / "Cargo.toml"))
            if bool(errors) == valid:
                raise AssertionError(f"unexpected result for {owner} -> {dependency}: {errors}")
            print(f"fixture {'accepted' if valid else 'rejected'}: {owner} -> {dependency} ({kind})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--expect-four", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
    data = metadata(args.manifest)
    if args.expect_four:
        members = {p["name"] for p in data["packages"] if p["id"] in data["workspace_members"]}
        if members != set(ALLOWED):
            parser.error(f"unexpected workspace members: {sorted(members)}")
    errors = violations(data)
    if errors:
        raise SystemExit("\n".join(errors))
    print(f"Dependency boundaries passed ({len(data['packages'])} resolved packages).")


if __name__ == "__main__":
    main()
