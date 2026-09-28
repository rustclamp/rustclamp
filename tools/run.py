#!/usr/bin/env python3
"""Run the coordinated test suite or Kernel microbench with clear status marks."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
COMMANDS = {
    "test": ["cargo", "test", "--offline", "--locked", "--workspace", "--all-features"],
    "bench": ["cargo", "bench", "--offline", "--locked", "-p", "rustclamp-kernel", "--bench", "resolution"],
}
COLORS = {"green": "32", "yellow": "33", "red": "31", "cyan": "36"}


def style(text, color, mode):
    enabled = mode == "always" or (
        mode == "auto" and "NO_COLOR" not in os.environ and sys.stdout.isatty()
    )
    if not enabled or mode == "never":
        return text
    return f"\033[{COLORS[color]}m{text}\033[0m"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=COMMANDS)
    parser.add_argument("--color", choices=("auto", "always", "never"), default="auto")
    args = parser.parse_args()
    command = COMMANDS[args.mode]
    print(style("▶", "cyan", args.color), " ".join(command), flush=True)

    suite = "Cargo tests"
    suite_count = 0
    test_count = 0
    failed_tests = 0
    bench_count = 0
    warnings = []
    try:
        process = subprocess.Popen(
            command,
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        assert process.stdout is not None
        for line in process.stdout:
            stripped = line.strip()
            bench_line = re.match(r"✓ BENCH (.+)", stripped)
            if args.mode == "bench" and bench_line:
                bench_count += 1
                print(style(f"✓ BENCH {bench_line.group(1)}", "green", args.color), flush=True)
                continue
            print(line, end="", flush=True)
            if stripped.startswith("Running "):
                suite = stripped.removeprefix("Running ").split(" (", 1)[0]
            elif stripped.startswith("Doc-tests "):
                suite = stripped
            result = re.search(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;", stripped)
            if result:
                passed, failed = map(int, result.groups())
                suite_count += 1
                test_count += passed
                failed_tests += failed
                mark = "✗" if failed else "✓"
                color = "red" if failed else "green"
                print(style(f"{mark} TEST SUITE {suite}: {passed} passed, {failed} failed", color, args.color), flush=True)
            if "warning:" in stripped.lower():
                warnings.append(stripped)
        return_code = process.wait()
    except OSError as error:
        print(style(f"✗ Could not start {args.mode}: {error}", "red", args.color))
        print(style(f"✗ {args.mode.upper()} FAILED", "red", args.color))
        return 1

    if args.mode == "test":
        if return_code or failed_tests:
            print(style(f"✗ TESTS FAILED ({test_count} passed, {failed_tests} failed)", "red", args.color))
            return return_code or 1
        if warnings:
            print(style(f"⚠ TESTS COMPLETED WITH WARNINGS ({test_count} passed, {suite_count} suites)", "yellow", args.color))
            for warning in warnings:
                print(style(f"⚠ {warning}", "yellow", args.color))
            return 0
        print(style(f"✓ TESTS SUCCESSFULLY COMPLETED ({test_count} passed, {suite_count} suites)", "green", args.color))
        return 0

    if return_code or bench_count == 0:
        print(style(f"✗ BENCH FAILED (exit={return_code}, measured_paths={bench_count})", "red", args.color))
        return return_code or 1
    print(style(f"⚠ BENCH COMPLETED WITH WARNINGS ({bench_count} paths measured)", "yellow", args.color))
    print(style("⚠ Single-host microbenchmark; timings are advisory and have no regression threshold.", "yellow", args.color))
    if warnings:
        for warning in warnings:
            print(style(f"⚠ {warning}", "yellow", args.color))
    return 0


if __name__ == "__main__":
    sys.exit(main())
