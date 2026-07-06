#!/usr/bin/env python3
"""Refresh a binary Homebrew formula in place for a new release.

Rewrites the `version` line and, for every per-platform `url` line pointing at a
GitHub release asset, retags the URL and recomputes the `sha256` on the line
that follows it (downloading the asset from the retagged URL). Everything else
in the formula -- desc, install block, test block -- is left untouched.

Usage: bump-tap-formula.py <tag> <formula-path>
  tag: the release tag, e.g. v0.2.1
"""
import hashlib
import re
import subprocess
import sys

RELEASE_URL = re.compile(
    r'^(\s*url\s+")'
    r'(https://github\.com/[^/]+/[^/]+/releases/download/)'
    r'[^/]+/'          # the old tag
    r'([^"]+)'         # the asset filename
    r'("\s*)$'
)


def sha256_of(url: str) -> str:
    # curl uses the system CA bundle, which is present both in CI and locally
    # (Python's urllib can miss it on some installs).
    data = subprocess.run(
        ["curl", "-fsSL", url], capture_output=True, check=True
    ).stdout
    return hashlib.sha256(data).hexdigest()


def main() -> int:
    tag, path = sys.argv[1], sys.argv[2]
    version = tag[1:] if tag.startswith("v") else tag

    lines = open(path, encoding="utf-8").read().split("\n")
    out = []
    i = 0
    while i < len(lines):
        line = lines[i]

        # version "X.Y.Z"
        if re.match(r'\s*version\s+"[^"]+"\s*$', line):
            out.append(re.sub(r'"[^"]+"', f'"{version}"', line, count=1))
            i += 1
            continue

        m = RELEASE_URL.match(line)
        if m:
            new_url = f"{m.group(2)}{tag}/{m.group(3)}"
            out.append(f"{m.group(1)}{new_url}{m.group(4)}")
            sha = sha256_of(new_url)
            print(f"  {m.group(3)} -> {sha}", file=sys.stderr)
            # the immediately following line is the matching sha256
            i += 1
            out.append(re.sub(r'"[0-9a-f]{64}"', f'"{sha}"', lines[i], count=1))
            i += 1
            continue

        out.append(line)
        i += 1

    open(path, "w", encoding="utf-8").write("\n".join(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
