# CLAUDE.md

`edikt` is a **lossless, format-preserving structured-config editor** for
**JSONC/JSON5**, **INI**, **TOML**, **YAML**, **KDL**, and **sectionless
key-value** files (`.env`, `.properties`, `zoo.cfg`-style), plus the
frontmatter block of Markdown files and PEP 723 script headers.

It edits with a **jq-flavored expression language** and a **sed-flavored
execution model** (stream-first, `-i` in place, `-e`/`-f` scripts). The one
thing it never does is reflow what it didn't touch: comments, indentation,
quoting, and trailing commas in every untouched region survive byte-for-byte.

**The behavior contract is [`docs/design/contract.md`](./docs/design/contract.md).**
Read the relevant section before changing behavior, and if a rule changes,
change it there *first*. Status and backlog: [`ROADMAP.md`](./ROADMAP.md).
Comment handling in depth:
[`docs/design/comments-as-first-class.md`](./docs/design/comments-as-first-class.md).

## The moat (non-negotiable)

Every format has a **lossless CST**: parse, then serialize, is byte-identical.
An edit touches only the nodes it targets; every untouched region is re-emitted
byte-for-byte. This is the entire reason the tool exists.

- No edit may alter indentation, line endings, comma style, quote style,
  comment placement, or trailing newline of any region it did not target. A
  new line takes the file's dominant ending.
- `parse ∘ serialize == identity` is a hard invariant, tested per format.
- We do **not** format, lint, or normalize. taplo and prettier own that.

## Rules that are easy to break

Each is spelled out, with its reason, in the contract.

- **Never silently drop data.** A lossy conversion warns on stderr; `--strict`
  makes it exit 2. A value the target cannot represent errors, naming the
  formats that can.
- **A mutation cannot combine with an output format** (`-T`, `--json`, ...):
  exit 2, "edit first, then convert".
- **A query miss exits 0**, sed-style. `--exit-status` opts into jq's exit 1.
- **Plain `=` auto-vivifies**, announced by a stderr `created` note; a scripted
  edit must assert that it applied. `--no-vivify` makes a missing path exit 2.
- **A structural get with output = input returns the original source slice**,
  not a re-emit.
- **`.env` is line-level editing only, forever**: no grammar, interpolation,
  quoting semantics, or storage coercion.
- **`envspaced` is never auto-detected**, and it is not an `ssh_config` parser.
- **A compact/flow target refuses an own-line comment** ("needs layout
  expansion") rather than reflowing bytes. KDL refuses wholesale replacement
  of a node body for the same reason; YAML instead writes a new mapping or
  sequence in the file's own layout (flow under flow, block at its indent).
- **Nothing is emitted in a spelling the file did not already use** (JSON5
  spellings in a `.jsonc`, a non-finite number into strict JSON errors).
- **Hyphenated bare keys work only on an assignment LHS or an object key.** In a
  query, `.a-b` is subtraction and must be quoted.
- **The frontmatter lens never touches the body**, and is input-only (`-T
  markdown` errors).
- **`^dN` is strict**: an out-of-range document is exit 2, not a no-op.
- **`sub`/`gsub` replacements use `$1`/`$name`**, a deliberate divergence from
  jq's string interpolation.
- **Builtins grow deliberately, never speculatively.** v1 has no `reduce`,
  `foreach`, `as $x`, `if/then/else`, or user functions; wanting them is a v2
  conversation.

## Crates

All in `crates/`; each format is isolated, with no cross-coupling. The
contract's Architecture section has the detail.

- `edikt`: the clap CLI (I/O, `-i`, mode dispatch, format detection, exit
  codes).
- `edikt-core`: `Value`, `Commented`, `Feature`, the expression language
  (lexer, parser, evaluator, builtins), and the `Document` trait, the one trait
  every format implements. Conversion is per-crate `emit`/`emit_commented`
  functions plus helpers in `edikt-core`'s `convert` module; there is no
  conversion trait.
- `edikt-syntax`: shared rowan helpers for `edikt-jsonc`, `edikt-ini`,
  `edikt-env`.
- `edikt-toml` (over `toml_edit`), `edikt-kdl` (over `kdl-rs`), `edikt-yaml`
  (span tree over `libyaml-safer`, byte-splice edits).
- `edikt-frontmatter`: the Markdown/PEP 723 lens over the YAML/TOML/JSONC
  crates.

**The crates are a public API.** Each format crate re-exports the `edikt-core`
types in its own signatures (`Value`, `Document`, `json!`, `parse as
parse_expr`, ...), so a dependent needs no direct `edikt-core` dependency. Keep
that true when a signature changes.

## Testing

- **Round-trip corpus**: every file under `fixtures/<format>/` must parse and
  serialize byte-identically. `fixtures/` keeps its exact bytes (the style
  check skips it).
- **Edit fixtures**: `(input, expr) -> expected_output`, asserting a minimal
  diff (a set changes exactly the targeted line).
- **Language tests** run the evaluator against the in-memory `Value` model,
  independent of any CST.
- **Conversion tests** cover the feature-derived warnings and the error cases.
- CLI behavior is tested end to end in `crates/edikt/tests/cli.rs`.
- There are no property tests yet; the identity invariant is fixture-tested.

Before pushing, run what CI runs: `cargo fmt --all -- --check`, `cargo clippy
--workspace --all-targets --all-features -- -D warnings`, and `cargo test
--workspace --all-features`.

## CI and release

CI is the rust-ci reusable workflows: see the thin callers in
`.github/workflows/`. Repo-specific: `style.yml` skips `LICENSE` and
`fixtures/`, and `release.yml`'s `crates:` list is in dependency order, so a
new crate goes in at its place in that order.

## Loop discipline

- Land work via **branch -> PR -> green CI -> merge** (not squash; tidy the
  commits instead). Never commit feature work straight to `main`; `release:`
  version-bump commits are the exception.
- **Apply formatting before committing** with `cargo fmt --all`, not just
  `--check`, and never gate on a *piped* check: `cargo fmt --check | tail && echo
  ok` reports the pipe's exit status (0), not fmt's, which once masked
  unformatted code into CI.
- Shared deps go through `[workspace.dependencies]`; crate **versions are
  per-crate**.
- A behavior change updates the contract, the clap doc comments (they are the
  `--help` text), and the README examples together.
- This repo is public: commit messages and files carry no session links.
