#!/usr/bin/env python3
"""Check and measure isolated Pico and equivalent plain Rust applications."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time
import tomllib

REPO = Path(__file__).resolve().parents[1]
CHANNEL = tomllib.loads((REPO / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
CARGO = ["cargo", f"+{CHANNEL}"]
PROFILE = '''[profile.release]
opt-level = 3
debug = false
strip = "none"
lto = false
codegen-units = 16
panic = "unwind"
'''


def run(command, cwd, env, capture=False):
    return subprocess.run(command, cwd=cwd, env=env, check=True, text=True,
                          stdout=subprocess.PIPE if capture else subprocess.DEVNULL).stdout


def prepare(root):
    shutil.copytree(REPO, root / "facade", ignore=shutil.ignore_patterns(".git", "target", "__pycache__"))
    for name in ("plain", "pico", "allocations"):
        path = root / name
        (path / "src").mkdir(parents=True)
        # Identical application package names avoid unnecessary crate-name effects.
        manifest = '[package]\nname="pico-cost-probe"\nversion="0.0.0"\nedition="2024"\npublish=false\n'
        manifest += '[workspace]\nresolver="3"\n' + PROFILE
        if name == "pico":
            manifest += '[dependencies]\nrustclamp={version="0.1", path="../facade"}\n'
            source = REPO / "examples/00-pico/main.rs"
        elif name == "allocations":
            manifest += '[dependencies]\nrustclamp={version="0.1", path="../facade", optional=true}\n[features]\nclamp=["dep:rustclamp"]\n'
            source = REPO / "benchmarks/fixtures/allocations.rs"
        else:
            source = REPO / "benchmarks/fixtures/plain.rs"
        (path / "Cargo.toml").write_text(manifest)
        shutil.copyfile(source, path / "src/main.rs")
    return {name: root / name for name in ("plain", "pico", "allocations")}


def environment(target):
    return {**os.environ, "CARGO_TARGET_DIR": str(target), "CARGO_INCREMENTAL": "1",
            "RUSTFLAGS": "", "CARGO_ENCODED_RUSTFLAGS": "",
            "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": ""}


def inspect(path, env, expected):
    data = json.loads(run(CARGO + ["metadata", "--offline", "--format-version", "1"], path, env, True))
    actual = {p["name"] for p in data["packages"]}
    if actual != expected:
        raise AssertionError(f"unexpected Pico graph: {actual}; expected {expected}")
    features = {p["name"]: next(n["features"] for n in data["resolve"]["nodes"] if n["id"] == p["id"])
                for p in data["packages"]}
    if any(features.values()):
        raise AssertionError(f"unexpected activated features: {features}")
    return {"packages": sorted(actual), "dependency_count": len(actual) - 1, "features": features}


def command(host, release=True, features=()):
    result = CARGO + ["build", "--offline", "--locked", "--target", host]
    if release:
        result += ["--release"]
    if features:
        result += ["--features", ",".join(features)]
    return result


def binary(target, host, release=True):
    return target / host / ("release" if release else "debug") / ("pico-cost-probe.exe" if os.name == "nt" else "pico-cost-probe")


def allocation_check(path, host):
    target = path / "target"
    env = environment(target)
    run(CARGO + ["generate-lockfile", "--offline"], path, env)
    run(CARGO + ["fmt", "--", "--check"], path, env)
    run(CARGO + ["clippy", "--offline", "--locked", "--all-features", "--", "-D", "warnings"], path, env)
    results = {}
    for release in (False, True):
        for name, features in (("plain", ()), ("pico", ("clamp",))):
            run(command(host, release, features), path, env)
            stdout = run([str(binary(target, host, release))], path, env, True)
            assert stdout.splitlines()[0] == "Hello Clamp"
            results[f'{name}_{"release" if release else "debug"}'] = json.loads(stdout.splitlines()[-1])
        assert results[f'plain_{"release" if release else "debug"}'] == results[f'pico_{"release" if release else "debug"}']
    return results


def elapsed(command_line, path, env):
    start = time.perf_counter_ns()
    run(command_line, path, env)
    return (time.perf_counter_ns() - start) / 1e9


def summarize(samples):
    ordered = sorted(samples)
    return {"samples": samples, "median": statistics.median(samples), "min": min(samples),
            "max": max(samples), "p95": ordered[min(len(ordered) - 1, int(len(ordered) * .95))]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--measure", type=Path, metavar="REPORT")
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--startup-repetitions", type=int, default=100)
    args = parser.parse_args()
    if args.repetitions < 3 or args.startup_repetitions < 10:
        parser.error("use at least 3 build and 10 process repetitions")
    compiler = subprocess.check_output(["rustc", f"+{CHANNEL}", "-vV"], text=True)
    host = next(line.split(": ", 1)[1] for line in compiler.splitlines() if line.startswith("host:"))
    with tempfile.TemporaryDirectory(prefix="clamp-pico-") as tmp:
        root = Path(tmp)
        paths = prepare(root)
        graphs = {}
        for name in ("plain", "pico"):
            expected = {"pico-cost-probe"} | ({"rustclamp"} if name == "pico" else set())
            graphs[name] = inspect(paths[name], environment(paths[name] / "target"), expected)
        allocations = allocation_check(paths["allocations"], host)
        measurements = {name: {key: [] for key in ("clean_seconds", "noop_seconds", "edited_seconds", "process_seconds", "binary_bytes")}
                        for name in ("plain", "pico")}
        binaries = {}
        for repetition in range(args.repetitions if args.measure else 1):
            for name in (("plain", "pico") if repetition % 2 == 0 else ("pico", "plain")):
                path = paths[name]
                target = root / f"target-{name}-{repetition}"
                env = environment(target)
                build = command(host)
                values = measurements[name]
                values["clean_seconds"].append(elapsed(build, path, env))
                values["noop_seconds"].append(elapsed(build, path, env))
                source = path / "src/main.rs"
                original = source.read_text()
                source.write_text(original + "\n// Controlled comment-only incremental edit.\n")
                values["edited_seconds"].append(elapsed(build, path, env))
                source.write_text(original)
                executable = binary(target, host)
                assert run([str(executable)], path, env, True) == "Hello Clamp\n"
                values["binary_bytes"].append(executable.stat().st_size)
                binaries[name] = executable
        if args.check:
            print(json.dumps({"graphs": graphs, "allocations": allocations}, indent=2))
            print("Isolated Pico graph, output, and allocation checks passed.")
            return
        for _ in range(5):
            for name in ("plain", "pico"):
                run([str(binaries[name])], paths[name], os.environ)
        for repetition in range(args.startup_repetitions):
            for name in (("plain", "pico") if repetition % 2 == 0 else ("pico", "plain")):
                measurements[name]["process_seconds"].append(elapsed([str(binaries[name])], paths[name], os.environ))
        report = {
            "schema_version": 1, "recorded_at": datetime.now(timezone.utc).isoformat(),
            "compiler": compiler, "cargo": subprocess.check_output(CARGO + ["-V"], text=True).strip(),
            "target": host, "machine": {"platform": platform.platform(), "cpu_count": os.cpu_count(),
                "cpu": next((line for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), "unknown") if Path("/proc/cpuinfo").exists() else platform.processor(),
                "memory": Path("/proc/meminfo").read_text().splitlines()[0] if Path("/proc/meminfo").exists() else "unknown",
                "load_average": os.getloadavg() if hasattr(os, "getloadavg") else None},
            "profile": tomllib.loads(PROFILE), "features": "default (none declared); isolated consumers",
            "environment": {k: environment(root)[k] for k in ("CARGO_INCREMENTAL", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER")},
            "commands": {"build": command(host), "process": "<fresh target>/<host>/release/pico-cost-probe; stdout=/dev/null",
                         "rerun": "python3 rustclamp/tools/pico.py --measure rustclamp/docs/evidence/reports/phase1-pico.json"},
            "cache": "Fresh target per clean sample; unchanged rebuild then comment-only source edit; compiler wrappers disabled; OS cache uncontrolled",
            "order": "Alternating plain/Pico order by repetition; 5 process warmups each",
            "build_repetitions": args.repetitions, "process_repetitions": args.startup_repetitions,
            "graphs": graphs, "allocations": allocations,
            "measurements": {name: {metric: summarize(samples) for metric, samples in values.items()} for name, values in measurements.items()},
            "inputs": {str(p.relative_to(REPO)): hashlib.sha256(p.read_bytes()).hexdigest() for p in
                       [REPO / "Cargo.toml", REPO / "src/lib.rs", REPO / "examples/00-pico/main.rs", REPO / "benchmarks/fixtures/plain.rs", REPO / "benchmarks/fixtures/allocations.rs", Path(__file__)]},
            "generated_manifests": {name: (path / "Cargo.toml").read_text() for name, path in paths.items()},
            "limitations": ["Process time includes spawn, stdout, and exit; does not isolate framework initialization.",
                "Allocation counts observe Rust global allocator calls on this compiler; optimization can eliminate allocations.",
                "No runtime CPU/RSS study: these one-line processes do not provide a sustained workload.",
                "Results are a local baseline, not a platform-independent zero-overhead guarantee."]}
        args.measure.parent.mkdir(parents=True, exist_ok=True)
        args.measure.write_text(json.dumps(report, indent=2) + "\n")
        print(args.measure)


if __name__ == "__main__":
    main()
