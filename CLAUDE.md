# edikt - build contract

`edikt` is a **lossless, format-preserving structured-config editor** for
**JSONC/JSON5**, **INI**, **TOML**, **YAML**, **KDL**, and **sectionless
key-value** files (`.env`, `.properties`, `zoo.cfg`-style).

It edits with a **jq-flavored expression language** and a **sed-flavored
execution model** (stream-first, `-i` in place, `-e`/`-f` scripts). The one
thing it never does is reflow what it didn't touch: comments, indentation,
quoting, and trailing commas in every untouched region survive byte-for-byte.

This file is the design contract. Read it before changing behavior. If you
change a rule here, change it here *first*.

---

## Step zero: the CST-fidelity spike (gating)

**Status: ✅ VALIDATED (2026-07-04).** The `spike/` crate (git-ignored) stands up
a minimal `rowan`+`logos` JSONC lexer/parser and proves, on a gnarly 274-byte
`tsconfig` (tab indent, trailing commas, `//` + `/* */` comments, nested
objects/arrays): byte-identical round-trip; a deep one-node edit changing exactly
one line; every untouched region byte-for-byte identical. INI round-trip + a
spacing-preserving value edit also pass. The `rowan` structural-sharing splice is
the mechanism: untouched nodes are the *same* green nodes, so byte-identity is
guaranteed by construction, not bookkeeping. **Build proceeds.**

The gating question this answered: prove the tree does lossless **edits**, not
just lossless parse. This is the fork between a weekend-per-format and a
fortnight-per-format; we now know it's the weekend side.

1. Stand up a minimal `rowan`+`logos` JSONC lexer/parser (objects, arrays,
   scalars, `//` and `/* */` comments, whitespace as trivia).
2. **Round-trip:** parse a gnarly commented `tsconfig.json` (nested, trailing
   commas, mixed indent, both comment styles) -> serialize -> **byte-identical**.
3. **One-node edit:** locate one deep value, splice a new value node, serialize
   -> **exactly one region differs**.
4. Repeat the round-trip probe for INI.
5. Benchmark `jsonc-parser` (dprint/David Sherret) as a baseline so the
   rowan choice is evidenced.

Pass -> build. If rowan editing proves ergonomically wrong for our splice
pattern, reconsider before writing more.

---

## Milestones

Live status and the full backlog live in [`ROADMAP.md`](./ROADMAP.md). In brief:

- ✅ **M0** Spike -> CST decision: rowan+logos, lossless edit proven.
- ✅ **M1** Skeleton + **query mode on JSONC** end-to-end (workspace, CLI,
  `Value`/expression language/evaluator, lossless CST + `Document` seam, output
  contract + exit codes).
- ✅ **M2** Mutation on JSONC: `=`, `|=`, `+=`, `del()` + the format-preserving CST
  **write** path (rowan splice) + `-i`. *The differentiator.*
- ✅ **M4** INI. ✅ **M5** `.env`/`.properties`. ✅ **M6** conversion (`-T`,
  Feature-driven warnings).
- ✅ **M8** TOML (lossless via `toml_edit`) and YAML (lossless via pure-Rust
  `libyaml-safer` span-tree splice): edit + query + convert.
- ✅ **Comment-preserving conversion** - the uniform head/inline/foot comment
  model, extracted and re-emitted by all seven formats.
- ✅ **KDL** - lossless via `kdl-rs`; the args/props/children projection convention.
- ✅ **M3** builtin/query polish (the regex family, `split`/`join`, affix
  predicates) and ✅ **M7** release infra (coverage, release workflow,
  packaging hooks; the release *ceremony* steps live in ROADMAP).

Realistic effort with fuller language + both formats + conversion: **3-5 weeks
part-time.** The language is the one thing that can balloon it; hold the v1
scope line.

---

## Testing

- **Round-trip corpus** per format: `parse ∘ serialize ≡ input`, byte-identical,
  over a fixture set of real-world gnarly files.
- **Edit fixtures**: `(input, expr) -> expected_output`, asserting minimal diffs
  (a set should change exactly the targeted line).
- **Language tests**: the evaluator against the in-memory `Value` model,
  independent of any CST.
- **Conversion tests**: feasibility lattice, including the error cases.
- Property test the identity invariant (`parse∘serialize`) where feasible.

---

## CI (house pattern)

Model on `jhheider/gpg-inspector` and `rpghearth/hearth-app` (good-netizen Rust
CI). Workflows to mirror:

- **check-and-lint** - `cargo check --workspace --all-features` with
  `RUSTFLAGS: -D warnings`; `cargo fmt --all -- --check`; `cargo clippy
  --workspace --all-targets --all-features` with `-D warnings`. `paths-ignore`
  docs; `concurrency` cancel-in-progress; `Swatinem/rust-cache`;
  `dtolnay/rust-toolchain@stable`.
- **test** - matrix over ubuntu/macos/windows: `cargo test --workspace
  --all-features`; plus a coverage job (`cargo-llvm-cov` -> lcov -> Coveralls).
- **audit** - `rustsec/audit-check` on a weekly cron + `workflow_dispatch`.
- **release** - on published GitHub Release: cross-platform binary matrix
  (linux x86_64/aarch64, macos x86_64/aarch64, windows x86_64) attached to the
  release, then `katyo/publish-crates` publishing workspace crates in dependency
  order.

Warnings are errors in CI, so the format-preservation invariants and the
round-trip corpus must be green before merge.

**Loop discipline & gotchas:**
- Every change lands via **branch -> PR -> green CI -> squash-merge**; never commit
  straight to `main`.
- **Apply formatting before committing** with `cargo fmt --all` (not just
  `--check`), and never gate on a *piped* check: `cargo fmt --check | tail && echo
  ok` reports the pipe's exit status (0), not fmt's, which masked unformatted
  code into CI once. (`let`-chains format fine on rustfmt 1.9; the earlier
  "avoid them" note was a misdiagnosis of that masked check.)
- Crates live in `crates/`; **fixtures in `fixtures/<format>/`** and every one
  must round-trip byte-identically. Shared deps go through
  `[workspace.dependencies]`; crate **versions are per-crate**.
