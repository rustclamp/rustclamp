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
    "rustclamp-messaging": {
        "itoa", "memchr", "proc-macro2", "quote", "serde", "serde_core",
        "serde_derive", "serde_json", "syn", "unicode-ident", "zmij",
    },
    "rustclamp-runtime": {
    'bytes',
    'errno',
    'libc',
    'mio',
    'pin-project-lite',
    'proc-macro2',
    'quote',
    'rustclamp-core',
    'signal-hook-registry',
    'socket2',
    'syn',
    'tokio',
    'tokio-macros',
    'unicode-ident',
    'wasi',
    'windows-link',
    'windows-sys',
    },
    "rustclamp-http": {
    'atomic-waker',
    'axum',
    'axum-core',
    'bitflags',
    'bytes',
    'errno',
    'form_urlencoded',
    'futures-channel',
    'futures-core',
    'futures-io',
    'futures-macro',
    'futures-sink',
    'futures-task',
    'futures-util',
    'http',
    'http-body',
    'http-body-util',
    'httparse',
    'httpdate',
    'hyper',
    'hyper-util',
    'itoa',
    'libc',
    'log',
    'matchit',
    'memchr',
    'mime',
    'mio',
    'once_cell',
    'percent-encoding',
    'pin-project-lite',
    'proc-macro2',
    'quote',
    'rustclamp-core',
    'ryu',
    'serde',
    'serde_core',
    'serde_derive',
    'serde_json',
    'serde_path_to_error',
    'serde_urlencoded',
    'signal-hook-registry',
    'slab',
    'smallvec',
    'socket2',
    'syn',
    'sync_wrapper',
    'tokio',
    'tokio-macros',
    'tower',
    'tower-http',
    'tower-layer',
    'tower-service',
    'tracing',
    'tracing-attributes',
    'tracing-core',
    'unicode-ident',
    'wasi',
    'windows-link',
    'windows-sys',
    'zmij',
    },
    "rustclamp-postgres": {
    'allocator-api2',
    'atoi',
    'autocfg',
    'base64',
    'bitflags',
    'block-buffer',
    'byteorder',
    'bytes',
    'cc',
    'cfg-if',
    'chacha20',
    'cmov',
    'cpufeatures',
    'crc',
    'crc-catalog',
    'crossbeam-queue',
    'crossbeam-utils',
    'crypto-common',
    'ctutils',
    'digest',
    'displaydoc',
    'dotenvy',
    'either',
    'equivalent',
    'errno',
    'etcetera',
    'event-listener',
    'find-msvc-tools',
    'foldhash',
    'form_urlencoded',
    'futures-channel',
    'futures-core',
    'futures-intrusive',
    'futures-io',
    'futures-macro',
    'futures-sink',
    'futures-task',
    'futures-util',
    'generic-array',
    'getrandom',
    'hashbrown',
    'hashlink',
    'heck',
    'hex',
    'hkdf',
    'hmac',
    'hybrid-array',
    'icu_collections',
    'icu_locale_core',
    'icu_normalizer',
    'icu_normalizer_data',
    'icu_properties',
    'icu_properties_data',
    'icu_provider',
    'idna',
    'idna_adapter',
    'indexmap',
    'itoa',
    'libc',
    'litemap',
    'lock_api',
    'log',
    'md-5',
    'memchr',
    'mio',
    'num-traits',
    'once_cell',
    'parking',
    'parking_lot',
    'parking_lot_core',
    'percent-encoding',
    'pin-project-lite',
    'potential_utf',
    'proc-macro2',
    'quote',
    'r-efi',
    'rand',
    'rand_core',
    'redox_syscall',
    'ring',
    'rustclamp-core',
    'rustls',
    'rustls-pki-types',
    'rustls-webpki',
    'scopeguard',
    'serde',
    'serde_core',
    'serde_derive',
    'serde_json',
    'sha2',
    'shlex',
    'signal-hook-registry',
    'slab',
    'smallvec',
    'socket2',
    'sqlx',
    'sqlx-core',
    'sqlx-macros',
    'sqlx-macros-core',
    'sqlx-postgres',
    'stable_deref_trait',
    'stringprep',
    'subtle',
    'syn',
    'synstructure',
    'thiserror',
    'thiserror-impl',
    'tinystr',
    'tinyvec',
    'tokio',
    'tokio-macros',
    'tokio-stream',
    'tracing',
    'tracing-attributes',
    'tracing-core',
    'typenum',
    'unicode-bidi',
    'unicode-ident',
    'unicode-normalization',
    'unicode-properties',
    'untrusted',
    'url',
    'utf8_iter',
    'version_check',
    'wasi',
    'webpki-roots',
    'whoami',
    'windows-link',
    'windows-sys',
    'windows-targets',
    'windows_aarch64_gnullvm',
    'windows_aarch64_msvc',
    'windows_i686_gnu',
    'windows_i686_gnullvm',
    'windows_i686_msvc',
    'windows_x86_64_gnu',
    'windows_x86_64_gnullvm',
    'windows_x86_64_msvc',
    'writeable',
    'yoke',
    'yoke-derive',
    'zerofrom',
    'zerofrom-derive',
    'zeroize',
    'zerotrie',
    'zerovec',
    'zerovec-derive',
    'zmij',
    },
}
OPTIONAL = {"rustclamp-runtime": {"tokio"}}


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
            if dep["name"] in OPTIONAL.get(name, set()) and not dep["optional"]:
                errors.append(f"{name} -> {dep['name']}: dependency must remain optional")
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
        ("rustclamp-runtime", "tokio", "optional", True),
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
    parser.add_argument("--expect-current-members", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
    data = metadata(args.manifest)
    if args.expect_current_members:
        members = {p["name"] for p in data["packages"] if p["id"] in data["workspace_members"]}
        if members != set(ALLOWED):
            parser.error(f"unexpected workspace members: {sorted(members)}")
    errors = violations(data)
    if errors:
        raise SystemExit("\n".join(errors))
    print(f"Dependency boundaries passed ({len(data['packages'])} resolved packages).")


if __name__ == "__main__":
    main()
