# edikt

[![CI](https://github.com/jhheider/edikt/actions/workflows/ci.yml/badge.svg)](https://github.com/jhheider/edikt/actions/workflows/ci.yml)
[![Coverage Status](https://coveralls.io/repos/github/jhheider/edikt/badge.svg?branch=main)](https://coveralls.io/github/jhheider/edikt?branch=main)
[![crates.io](https://img.shields.io/crates/v/edikt.svg)](https://crates.io/crates/edikt)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**Edit config files without reflowing them.**

You want to change one value in a commented, hand-formatted config. Today every
option costs you something:

```console
$ jq '.compilerOptions.target = "ES2022"' tsconfig.json
# comments gone, trailing commas gone, the whole file reindented,
# and JSONC-with-comments won't round-trip through jq at all
```

edikt changes the one value and nothing else:

```console
$ edikt -i '.compilerOptions.target = "ES2022"' tsconfig.json
# one line of the diff moves; every comment, blank line, indent, and
# trailing comma comes back byte for byte
```

*edit, meets edict.* A lossless, format-preserving editor for **JSONC/JSON5**,
**TOML**, **YAML**, **INI**, **KDL**, and flat key-value files (**`.env`**,
**`.properties`**), driven by a jq-flavored expression language and a sed-shaped
execution model. It touches only the bytes you point at.

```sh
# query - reads like jq
edikt '.compilerOptions.strict' tsconfig.json

# edit in place - comments, indent, comma style all preserved
edikt -i '.compilerOptions.target = "ES2022"' tsconfig.json

# edit YAML in place - anchors, flow style, and comments survive
edikt -i '.services.web.replicas = 3' compose.yaml

# compute, not just place
edikt -i '.version |= . + "-dev"' package.jsonc

# stream-first, like sed (stdin has no extension, so name the format)
cat settings.jsonc | edikt -t jsonc 'del(.telemetry) | .theme = "dark"'

# script from a file
edikt -f release.edk -i config.jsonc

# convert, where feasible - comments carried across, in the target's syntax
edikt -T yaml tsconfig.jsonc

# Markdown frontmatter - edit the metadata block, the prose untouched
edikt -i '.draft = false' post.md

# multi-document YAML - one edit maps over every ---document in the stream
edikt -i '.metadata.labels.env = "prod"' k8s.yaml

# ...or target one document by position with ^dN (0-based, strict)
edikt -i '^d1 | .spec.replicas = 3' k8s.yaml

# PEP 723 / scriptbox blocks - bump the pin, the code below untouched
edikt -t frontmatter -i '.["requires-python"] = ">=3.12"' app.py
```

Every one of these writes back a file that is byte-identical except for the
value you changed, the kind of diff a reviewer reads at a glance.

## Beyond config files: frontmatter and streams

**Markdown frontmatter.** For a `.md`/`.markdown`/`.mdx`/`.qmd` file (or
`-t markdown`), edikt edits the leading metadata block, YAML (`---`), TOML
(`+++`), tagged (`---json`), or Hugo bare-brace JSON, and leaves the document
body byte-for-byte opaque. `edikt '.title' post.md` queries it;
`edikt -i '.draft = false' post.md` rewrites one key with the prose intact.

**Commented host-language blocks.** edikt reads PEP 723 `# /// script` ... `# ///`
blocks in a Python file (uv) or a shell script (scriptbox), bumps the pin, and
re-applies the `# ` prefix with the code below untouched.

**Multi-document YAML.** A `---`-separated stream (Kubernetes manifests, Ansible,
Helm output) is first-class. An edit maps over every document by default and
silently skips any that lack the target path; a query returns one result per
document. `select(pred)` targets documents by content
(`select(.kind == "Service") | .spec.type = "LoadBalancer"`), and `^dN` selects
one by position (`^d0` is the first), strict there, so `^d5` on a
three-document stream is an error, not a no-op.

## Install

```sh
brew install jhheider/tap/edikt   # Homebrew
cargo install edikt               # Cargo (crates.io)
pkgx install edikt                # pkgx (builds from source)
```

Or grab a prebuilt binary (Linux, macOS, and Windows; x86_64 and arm64) from the
[releases page](https://github.com/jhheider/edikt/releases).

## A worked example: bumping a Cargo workspace

edikt is happy editing the config it ships in. Here is the whole version bump
for a release, a `Cargo.toml` per crate plus the internal dependency pins in
the workspace root, done losslessly:

```sh
# each crate's own version
for c in crates/*/Cargo.toml; do
  edikt -i '.package.version = "1.2.3"' "$c"
done

# the [workspace.dependencies] pins (inline tables), chained with -e
edikt -i \
  -e '.workspace.dependencies."edikt-core".version   = "1.2.3"' \
  -e '.workspace.dependencies."edikt-syntax".version = "1.2.3"' \
  Cargo.toml
```

Each edit moves exactly one `version = "..."`. The `# Internal crates` comment
above the dependency table, the `path = "crates/edikt-core"` sitting next to
each version, the brace-and-space style of every inline table, and every other
byte come back unchanged, the kind of diff a reviewer reads at a glance.

One quoting note, and it only bites queries. A hyphenated key needs no quoting
wherever it cannot be arithmetic: on the **left of an assignment** (a target must
be a path) and as an **object key** (a literal name, never an expression). So
`edikt -i '.dev-dependencies.serde = {version: "1", default-features: false}'`
just works. In a **query** the same text is genuinely ambiguous, since
`.total-length` is a real subtraction of the `length` builtin, so write
`."dev-dependencies"` or `.["dev-dependencies"]`. edikt says which and why rather
than guessing.

## Why edikt?

Honestly, a weekend project, one that started with a real itch. The problem
crystallized while reading [*"Respectful" YAML Patching in
Rust*](https://verrchu.github.io/blog/2-respectful-yaml-patching-in-rust/),
which surveys the Rust libraries for patching YAML and lands on the gap in one
line: **none of them preserve both** the formatting *and* the comments. That's
the exact thing I kept wanting, surgically change one value (or one comment)
and leave every other byte, comment, and blank line alone, and not just for
YAML but for the whole pile of config formats a project accumulates.

The good tools each own their corner: `jq` and `yq` for querying, `taplo` and
`prettier` for formatting, and the excellent `toml_edit` and `kdl-rs` crates for
lossless edits (edikt is *built on* those last two). edikt isn't trying to
replace them, it's the piece I couldn't find off the shelf: one jq-flavored
tool that edits **and** queries **and** converts across JSONC, INI, TOML, YAML,
KDL, and `.env`, touching only the bytes you point at. If your need is
single-format, reach for the specialist; if it's "the same surgical edit, across
all of these," that's the gap this fills.

## What edikt won't do

It edits values and comments; it does not reformat, lint, or validate schemas,
reach for `taplo`/`prettier` for that. The expression language is a curated
subset of jq (navigation, mutation, arithmetic, `//` defaults, regex,
`split`/`join`); `if/then`, `reduce`, variables (`as $x`), and user-defined
functions are not in v1. And `.env` is flat and string-valued, always. Stating
the boundaries plainly is the point: inside them, the surgical-edit promise
holds byte for byte.

## Status

Seven config formats with lossless in-place edit, query, and comment-preserving
conversion, plus a frontmatter lens (Markdown and PEP 723 host-language blocks)
and multi-document YAML streams with `select`/`^dN` targeting. On crates.io,
Homebrew, and pkgx, the badge above tracks the current version. See
[`CLAUDE.md`](./CLAUDE.md) for the build contract.

## Scripting notes

- **Exit codes are sed-shaped:** `0` = success, a query that matches nothing
  is a *silent no-op* (safe under `set -e`); `2` = parse or evaluation error.
  A missing key therefore prints nothing and exits `0`, which is deliberate -
  it is sed with no matching address, not an error. To tell "absent" from
  "present but empty", ask for it:

  ```bash
  # presence test: --exit-status opts into jq's 1-on-no-results
  if edikt --exit-status '.feature.enabled' config.yaml >/dev/null; then
    echo "key is present"
  fi

  # or supply a default in the expression with //
  edikt '.maybe.key // "fallback"' f.yaml
  ```
- **The expression language is deliberately capped in v1.** jq's navigation,
  mutation, arithmetic, `//` defaults, and a curated builtin registry
  (including regex `test`/`match`/`capture`/`sub`/`gsub`, `split`/`join`) are
  in; variables (`as $x`), `if/then`, `reduce`, and user-defined functions are
  not (yet).
- **Many files, sed-style:** `edikt -i '.v = 9' a.json b.json` (let the shell
  glob: `edikt -i 'del(.telemetry)' config/*.jsonc`). Queries over several
  files concatenate results in order.
- **`envspaced` is the same model with a space separator**, for
  `sshd_config`-shaped daemon configs: `edikt -t envspaced -i '.Port = 2222'
  sshd_config`. The first run of spaces or tabs ends the key and everything
  after it is the value, so `Subsystem sftp /usr/lib/sftp-server` reads as one
  value, and a tab-separated line stays tab-separated. It is never auto-detected
  - `sshd_config` has no extension, `.conf` already means INI, and a `key value`
  line is indistinguishable from a malformed `.env` line - so it is `-t
  envspaced` or nothing. It is deliberately *not* an `ssh_config` parser:
  `Match` / `Host` blocks scope the keys beneath them and this model is flat, so
  a file using them is out of scope rather than half-supported.
- **`.env` is flat and string-valued**, no arrays or nesting, ever, but
  string computation on values works fine:
  `edikt -i '.VERSION |= sub("^v"; "")' .env`.
- **`.env` quotes are part of the value, not syntax.** There is no single `.env`
  grammar (docker-compose, dotenv libraries and shell `source` disagree), so
  edikt interprets nothing: for `APP_NAME="my app"` the value is the seven-plus
  characters `"my app"` *including* the quotes, and assigning replaces the whole
  run of bytes after the `=`. To keep the quotes, include them in the new value:

  ```bash
  edikt '.APP_NAME' .env                       # => "my app"   (quotes included)
  edikt -i '.APP_NAME = "my app"'   .env       # => APP_NAME=my app
  edikt -i '.APP_NAME = "\"my app\""' .env     # => APP_NAME="my app"
  ```

  Everything edikt did not target keeps its bytes, so setting a value to exactly
  its current text is a no-op on the file.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
