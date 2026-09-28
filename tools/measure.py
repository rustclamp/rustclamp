#!/usr/bin/env python3
"""Record repeatable Cargo build measurements; run from the pinned workspace."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time
import tomllib


def output(command, cwd):
    return subprocess.check_output(command, cwd=cwd, text=True).strip()


def timed(command, cwd, env):
    start = time.perf_counter_ns()
    subprocess.run(command, cwd=cwd, env=env, check=True, stdout=subprocess.DEVNULL)
    return (time.perf_counter_ns() - start) / 1_000_000_000


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--example", help="Optional executable Cargo example to build and run")
    parser.add_argument("--features", default="", help="Comma-separated features; defaults are disabled")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.repetitions < 2:
        parser.error("use at least two repetitions")
    manifest = args.manifest.resolve()
    cwd = manifest.parent
    feature_args = ["--no-default-features"]
    if args.features:
        feature_args += ["--features", args.features]
    metadata_command = ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
                        "--manifest-path", str(manifest), *feature_args]
    data = json.loads(output(metadata_command, cwd))
    package = next(p for p in data["packages"] if p["name"] == args.package)
    nodes = {n["id"]: n for n in data["resolve"]["nodes"]}
    reached = set()
    pending = list(nodes[package["id"]]["dependencies"])
    while pending:
        dep = pending.pop()
        if dep not in reached:
            reached.add(dep)
            pending.extend(nodes[dep]["dependencies"])
    command = ["cargo", "build", "--offline", "--locked", "--release", "--manifest-path",
               str(manifest), "--package", args.package, *feature_args]
    if args.example:
        command += ["--example", args.example]
    clean, incremental, startup, sizes = [], [], [], []
    compiler = output(["rustc", "-vV"], cwd)
    host = next(line.split(": ", 1)[1] for line in compiler.splitlines() if line.startswith("host:"))
    # Always fix the target so host-dependent Cargo defaults cannot change the run.
    command += ["--target", host]
    with tempfile.TemporaryDirectory(prefix="clamp-measure-") as tmp:
        for repetition in range(args.repetitions):
            target = Path(tmp) / str(repetition)
            env = {**os.environ, "CARGO_TARGET_DIR": str(target), "CARGO_INCREMENTAL": "1"}
            clean.append(timed(command, cwd, env))
            incremental.append(timed(command, cwd, env))
            if args.example:
                binary = target / host / "release/examples" / (args.example + (".exe" if os.name == "nt" else ""))
                sizes.append(binary.stat().st_size)
                startup.append(timed([str(binary)], cwd, env))
    source_root = Path(package["manifest_path"]).parent
    workspace_manifest = Path(data["workspace_root"]) / "Cargo.toml"
    workspace_settings = tomllib.loads(workspace_manifest.read_text())
    digest = hashlib.sha256()
    for path in sorted(source_root.rglob("*")):
        if (
            path.is_file()
            and path.suffix.lower() != ".md"
            and not {".git", "target", "__pycache__", "reports"}.intersection(path.relative_to(source_root).parts)
        ):
            digest.update(str(path.relative_to(source_root)).encode())
            digest.update(path.read_bytes())
    report = {
        "schema_version": 1, "recorded_at": datetime.now(timezone.utc).isoformat(),
        "package": args.package, "source_sha256": digest.hexdigest(),
        "toolchain": compiler, "cargo": output(["cargo", "-V"], cwd), "target": host,
        "machine": {"platform": platform.platform(), "cpu_count": os.cpu_count(),
                    "cpu": Path("/proc/cpuinfo").read_text().split("model name", 1)[-1].splitlines()[0].strip() if Path("/proc/cpuinfo").exists() else platform.processor(),
                    "memory": Path("/proc/meminfo").read_text().splitlines()[0] if Path("/proc/meminfo").exists() else "unavailable"},
        "profile": {"name": "release", "explicit_settings": workspace_settings.get("profile", {}).get("release", {})},
        "features": {"default": False, "explicit": args.features},
        "environment": {key: os.environ.get(key, "") for key in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"]},
        "build_configuration": "Cargo manifests and .cargo/config files are authoritative; archive alongside report",
        "workspace_manifest_sha256": hashlib.sha256(workspace_manifest.read_bytes()).hexdigest(),
        "lockfile_sha256": hashlib.sha256((workspace_manifest.parent / "Cargo.lock").read_bytes()).hexdigest(),
        "cache": "Fresh target directory per clean sample; OS page cache uncontrolled; immediate unchanged rebuild; CARGO_INCREMENTAL=1",
        "repetitions": args.repetitions, "commands": {"build": command, "metadata": metadata_command},
        "dependency_count": len(reached),
        "clean_seconds": clean, "clean_median_seconds": statistics.median(clean),
        "incremental_noop_seconds": incremental, "incremental_noop_median_seconds": statistics.median(incremental),
        "binary_bytes": sizes or None, "process_wall_seconds": startup or None,
        "runtime_allocations": None, "runtime_memory": None, "runtime_cpu": None,
        "limitations": (["No executable was measured; pass --example to record executable metrics."] if not args.example else []) +
                       ["Process wall time includes spawn, program execution, output redirection and exit; it does not isolate initialization.",
                        "Allocation counts require a separate instrumented prototype; CPU and memory need a workload-specific measurement.",
                        "Incremental samples are unchanged builds, not edited-source recompilation."]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(args.output)


if __name__ == "__main__":
    main()
