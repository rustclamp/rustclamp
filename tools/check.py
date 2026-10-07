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
    "rustclamp-http": "http",
    "rustclamp-postgres": "postgres",
    "rustclamp-messaging": "messaging",
    "rustclamp-worker": "worker",
    "rustclamp-scheduler": "scheduler",
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
        # [patch] redirects a git dependency to a sibling checkout (#86).
        sections.extend(package.get("patch", {}).values())
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


def has_path_dependency(package_dir):
    """True when the manifest names any path dependency, sibling repo or in-repo (e.g. macros)."""
    package = tomllib.loads((package_dir / "Cargo.toml").read_text())
    sections = [package.get(section, {}) for section in ("dependencies", "build-dependencies")]
    sections.extend(
        target.get(section, {})
        for target in package.get("target", {}).values()
        for section in ("dependencies", "build-dependencies")
    )
    return any(isinstance(declaration, dict) and "path" in declaration
               for section in sections for declaration in section.values())


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


def resolved_dependencies(metadata, package_name):
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
    return {packages[item] for item in reached}


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
    # Learning path first (00-pico runs in check_pico_runtime_absence), then the architecture track.
    examples = (
        ("01-users", "rustclamp-example-users"),
        ("02-axum-app", "rustclamp-example-axum-app"),
        ("03-email-worker", "rustclamp-example-email-worker"),
        ("04-scheduler", "rustclamp-example-scheduler"),
        ("05-messaging", "rustclamp-example-messaging"),
        ("06-create-order", "rustclamp-example-create-order"),
        ("07-device-loop", "rustclamp-example-device-loop"),
        ("08-site-server", "rustclamp-example-site-server"),
        ("09-observability", "rustclamp-example-observability"),
        ("a1-capability", "rustclamp-example-capability"),
        ("a2-module", "rustclamp-example-module"),
        ("a3-contribution", "rustclamp-example-contribution"),
        ("a4-process", "rustclamp-example-process"),
        ("a5-lifecycle", "rustclamp-example-lifecycle"),
        ("a6-platform-neutral", "rustclamp-example-platform-neutral"),
    )
    for example, package_name in examples:
        example_root = root / "rustclamp/examples" / example
        manifest = example_root / "Cargo.toml"
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
             "--manifest-path", str(manifest)], cwd=root, text=True))
        dependencies = resolved_dependencies(metadata, package_name)
        assert "rustclamp" not in dependencies, f"{example} unexpectedly depends on the facade"
        if example in {"a4-process", "a5-lifecycle", "06-create-order", "07-device-loop"}:
            assert "rustclamp-tooling" not in dependencies, (
                f"{example} activated optional inspection tooling by default"
            )
        if example == "07-device-loop":
            assert "tokio" not in dependencies, "default device loop activated Tokio"
        if example == "a6-platform-neutral":
            assert "rustclamp-kernel" not in dependencies, "platform-neutral consumer activated Kernel"
        print(f"{example} dependency graph: {', '.join(sorted(dependencies))}; facade absent")

        run("cargo", "fmt", "--manifest-path", str(manifest), "--", "--check", cwd=root)
        run("cargo", "clippy", "--offline", "--locked", "--manifest-path", str(manifest),
            "--all-targets", "--", "-D", "warnings", cwd=root)
        run("cargo", "test", "--offline", "--locked", "--manifest-path", str(manifest), cwd=root)
        if example == "07-device-loop":
            run("cargo", "clippy", "--offline", "--locked", "--manifest-path", str(manifest),
                "--all-targets", "--all-features", "--", "-D", "warnings", cwd=root)
            run("cargo", "test", "--offline", "--locked", "--manifest-path", str(manifest),
                "--all-features", cwd=root)
            run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
                "--example", "07-device-loop-direct", cwd=root)
            run("cargo", "build", "--offline", "--locked", "--release", "--manifest-path",
                str(manifest), "--examples", cwd=root)
            run("cargo", "bench", "--offline", "--locked", "--manifest-path", str(manifest),
                "--bench", "loop", cwd=root)
        if example not in {"01-users", "06-create-order", "08-site-server"}:
            run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
                "--example", example, cwd=root)
        if example == "a5-lifecycle":
            assert "tokio" not in dependencies, "Tokio activated in the default lifecycle example"
            feature_metadata = json.loads(subprocess.check_output(
                ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
                 "--manifest-path", str(manifest), "--features", "tokio-runtime"],
                cwd=root, text=True))
            feature_dependencies = resolved_dependencies(feature_metadata, package_name)
            assert "tokio" in feature_dependencies, "Tokio feature did not activate Tokio"
            run("cargo", "clippy", "--offline", "--locked", "--manifest-path", str(manifest),
                "--all-targets", "--all-features", "--", "-D", "warnings", cwd=root)
            run("cargo", "test", "--offline", "--locked", "--manifest-path", str(manifest),
                "--all-features", cwd=root)
        if example == "01-users":
            assert {"rustclamp-http", "rustclamp-postgres", "tokio"}.isdisjoint(dependencies), (
                "console-only Users graph activated an optional integration"
            )
            run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
                "--bin", "users-console", "--", "create", "Ada", cwd=root)
            for feature in (
                "http", "postgres", "tracing", "measure-allocations",
                "http,postgres,tracing,measure-allocations",
            ):
                run("cargo", "clippy", "--offline", "--locked", "--manifest-path", str(manifest),
                    "--features", feature, "--all-targets", "--", "-D", "warnings", cwd=root)
                run("cargo", "test", "--offline", "--locked", "--manifest-path", str(manifest),
                    "--features", feature, "--all-targets", cwd=root)
        if example == "a3-contribution":
                run("cargo", "bench", "--offline", "--locked", "--manifest-path", str(manifest),
                    "--bench", "assembly", cwd=root)


def check_process_build_targets(root):
    manifest = root / "rustclamp/examples/a4-process/build-targets/Cargo.toml"
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


def check_pico_runtime_absence(root):
    manifest = root / "rustclamp/Cargo.toml"
    graph = subprocess.check_output(
        ["cargo", "tree", "--offline", "--locked", "--manifest-path", str(manifest), "-e", "features"],
        cwd=root, text=True,
    )
    assert "tokio" not in graph.lower(), f"Pico activated Tokio:\n{graph}"
    run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
        "--example", "00-pico", cwd=root)
    print("isolated Pico: Tokio feature absent")


def check_tooling(root):
    manifest = root / "rustclamp/tooling/Cargo.toml"
    run("cargo", "fmt", "--manifest-path", str(manifest), "--", "--check", cwd=root)
    run("cargo", "clippy", "--offline", "--locked", "--manifest-path", str(manifest),
        "--all-targets", "--", "-D", "warnings", cwd=root)
    run("cargo", "test", "--offline", "--locked", "--manifest-path", str(manifest), cwd=root)
    run("cargo", "doc", "--offline", "--locked", "--manifest-path", str(manifest),
        "--no-deps", cwd=root, env={**os.environ, "RUSTDOCFLAGS": "-D warnings"})
    run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
        "--", "--help", cwd=root)
    run("cargo", "run", "--offline", "--locked", "--manifest-path", str(manifest),
        "--", "check", "--offline", "--manifest-path", str(manifest), cwd=root)
    check_tooling_generator(root)


def check_tooling_generator(root):
    tooling_manifest = root / "rustclamp/tooling/Cargo.toml"
    facade = root / "rustclamp"
    git_dependency = 'git = "https://github.com/rustclamp/rustclamp", branch = "main"'
    with tempfile.TemporaryDirectory(prefix="clamp-init-") as temporary:
        for template, flags, expected, run_args in (
            ("blank", ["--blank"], "Hello from Clamp!", []),
            ("app", ["--app"], "Health check: ready", []),
            ("tui", ["--tui"], "Clamp TUI", []),
            ("web", ["--web"], None, []),  # ponytail: long-running server, checked and tested only
            ("package", ["--package"], None, []),  # a library: nothing to run
            # Profiles (ADR 0032); the service runs only its in-process route test.
            ("cli", ["--profile", "cli"], "Hello from Clamp!", []),
            ("worker", ["--profile", "worker"], "worker idle", ["--once"]),
            ("combined", ["--profile", "cli,service,worker"], "worker idle", ["work", "--once"]),
        ):
            project = Path(temporary) / template
            command = [
                "cargo", "run", "--offline", "--locked", "--manifest-path",
                str(tooling_manifest), "--", "init", str(project), *flags,
            ]
            run(*command, cwd=root)

            manifest = project / "Cargo.toml"
            contents = manifest.read_text()
            assert git_dependency in contents, f"{template} did not use the public facade dependency"
            contents = contents.replace(git_dependency, f'path = "{facade}"')
            manifest.write_text(contents)

            run("cargo", "check", "--offline", "--manifest-path", str(manifest), cwd=root)
            run("cargo", "test", "--offline", "--manifest-path", str(manifest), cwd=root)
            if template == "package":
                for name in ("src/lib.rs", "resources/views/index.html", "resources/css/package.css",
                             "tests/routes.rs"):
                    assert (project / name).exists(), f"package template omitted {name}"
                assert not (project / "Procfile.dev").exists(), "package template wrote a Procfile"
            if template == "web":
                for name in ("package.json", "vite.config.ts", "app/routes/web.rs", "app/routes/api.rs", "app/http/kernel.rs", "tests/routes.rs",
                             "app/resources/views/welcome.html", "public/robots.txt", ".env.example"):
                    assert (project / name).exists(), f"web template omitted {name}"
            if expected:
                output = subprocess.check_output(
                    ["cargo", "run", "--offline", "--manifest-path", str(manifest), "--", *run_args],
                    cwd=root, text=True, input="",
                )
                assert expected in output, f"{template} output omitted {expected!r}: {output}"
            global CHECKS_PASSED
            CHECKS_PASSED += 1
            print(f"generated {template} project checked, tested" + (", and ran" if expected else ""))


def compare_tooling_projection(root, example, export_args, expected_processes):
    example_manifest = root / f"rustclamp/examples/{example}/Cargo.toml"
    tooling_manifest = root / "rustclamp/tooling/Cargo.toml"
    document_text = subprocess.check_output(
        ["cargo", "run", "--quiet", "--offline", "--locked", "--manifest-path",
         str(example_manifest), *export_args],
        cwd=root, text=True)
    document = json.loads(document_text)
    processes = {process["id"]: process for process in document["processes"]}
    assert set(processes) == set(expected_processes), (
        f"{example} exported unexpected process IDs: {sorted(processes)}"
    )

    def clamp(*arguments):
        return subprocess.check_output(
            ["cargo", "run", "--quiet", "--offline", "--locked", "--manifest-path",
             str(tooling_manifest), "--", *arguments], cwd=root, text=True)

    with tempfile.TemporaryDirectory(prefix="clamp-inspection-") as temporary:
        path = Path(temporary) / "architecture.json"
        path.write_text(document_text)
        for process_id, process in processes.items():
            summary = clamp("inspect", str(path), "--process", process_id)
            assert f"Process: {process_id}" in summary
            assert f"Included modules: {len(process['included_modules'])}" in summary

            tree = clamp("tree", str(path), "--process", process_id)
            for module in process["included_modules"]:
                assert module["id"] in tree, f"tree omitted {module['id']} from {process_id}"

            graph = clamp("graph", str(path), "--process", process_id)
            for module in process["included_modules"]:
                for parent, child in zip(module["path"], module["path"][1:]):
                    expected = f"{parent} -> {child} [inclusion]"
                    assert expected in graph, f"graph omitted {expected}"
            for requirement in process["requirements"]:
                provider = requirement["provider"]
                if provider is not None:
                    expected = (
                        f"{requirement['consumer']} -> {provider} "
                        f"[capability:{requirement['capability']}]"
                    )
                    assert expected in graph, f"graph omitted {expected}"
            for contribution in process["contributions"]:
                expected = (
                    f"{contribution['contributor']} -> {contribution['consumer']} "
                    f"[contribution:{contribution['target']}]"
                )
                assert expected in graph, f"graph omitted {expected}"

            included = next(
                module for module in process["included_modules"]
                if module["reason"]["kind"] == "capability_provider"
            )
            explanation = clamp("why", str(path), included["id"], "--process", process_id)
            assert " -> ".join(included["path"]) in explanation

            if process["exclusions"]:
                excluded = process["exclusions"][0]
                explanation = clamp("why", str(path), excluded["id"], "--process", process_id)
                assert excluded["reason"] in explanation
        print(f"clamp inspect/tree/graph/why match the {example} projections")


def check_tooling_reference(root):
    compare_tooling_projection(
        root, "a4-process", ["--example", "inspection-json", "--features", "tooling-inspection"],
        ["example.process.cli", "example.process.worker"],
    )
    compare_tooling_projection(
        root, "a5-lifecycle", ["--example", "inspection-json", "--features", "tooling-inspection"],
        ["example.lifecycle.worker", "example.lifecycle.reporter"],
    )
    compare_tooling_projection(
        root, "07-device-loop", ["--example", "inspection-json", "--features", "tooling-inspection"],
        ["example.device-loop.simulation"],
    )
    compare_tooling_projection(
        root, "06-create-order",
        ["--bin", "phase7-inspect", "--features", "tooling-inspection", "--", "--tooling-json"],
        ["orders-api", "outbox-publisher", "orders-worker", "orders-scheduler"],
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        run("python3", str(root / "rustclamp/tools/workspace.py"), "--root", str(root), cwd=root)
        run("cargo", "generate-lockfile", "--offline", cwd=root)
        run("python3", str(root / "rustclamp/tools/boundaries.py"), "--manifest",
            str(root / "Cargo.toml"), "--expect-current-members", cwd=root)
        check(root)
        check_pico_runtime_absence(root)
        check_examples(root)
        check_process_build_targets(root)
        check_tooling(root)
        check_tooling_reference(root)
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
                if repo == "rustclamp":
                    check_pico_runtime_absence(isolated_root)
                if local_dependencies or has_path_dependency(isolated):
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
