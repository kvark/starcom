#!/usr/bin/env python3
"""Reject native SSH/crypto backends in the effective all-feature target graph."""
import json
import subprocess
import sys

FORBIDDEN = {
    "ssh2", "libssh2-sys", "openssl", "openssl-sys", "openssl-src", "native-tls",
    "ring", "aws-lc-rs", "aws-lc-sys", "aws-lc-fips-sys", "mbedtls", "mbedtls-sys-auto",
}


def main():
    version = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.split(": ", 1)[1] for line in version.splitlines() if line.startswith("host: "))
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--format-version", "1",
        "--all-features", "--filter-platform", host,
    ], text=True))
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    remaining = [metadata["resolve"]["root"]]
    reachable = set()
    while remaining:
        node = remaining.pop()
        if node in reachable:
            continue
        reachable.add(node)
        remaining.extend(dep["pkg"] for dep in nodes[node]["deps"])
    forbidden = sorted({packages[node]["name"] for node in reachable} & FORBIDDEN)
    if forbidden:
        raise RuntimeError("native crypto dependencies are forbidden: " + ", ".join(forbidden))
    print(f"Dependency policy passed for {host}: {len(reachable) - 1} resolved dependencies; no native SSH/crypto backend.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError, KeyError, StopIteration) as error:
        print(f"Dependency policy failed: {error}", file=sys.stderr)
        sys.exit(1)
