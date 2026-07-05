# edikt roadmap

Backlog and direction. The build contract (invariants, CLI, architecture) lives
in [`CLAUDE.md`](./CLAUDE.md); this file is the sequencing.

## Status

- ✅ **M0** — CST-fidelity spike (rowan+logos lossless edit proven).
- ✅ **M1** — query mode on JSONC end-to-end: workspace + CI, `edikt-core`
  (Value, Feature, expression language, evaluator), `edikt-syntax` + `edikt-jsonc`
  (lossless CST, `Document` seam), and the `edikt` CLI.
- ✅ **M2 — Mutation.** `set` (`=`, `|=`), `del()`, `+=`/append — all
  format-preserving via the rowan splice — plus `-i` and a format-agnostic CLI.
- ✅ **M4 — INI.** Line-oriented lossless CST, `.section.key` paths, inline
  comments, set/del, `FEATURES = [Comments, Sections]`.
- ✅ **M5 — `.env` / `.properties`.** Flat, string-valued, line-level editing,
  interpret-nothing; `.env` dotfile detection; `FEATURES = [Comments]`.
- ✅ **M6 — Conversion (`-T`).** Parse → value → emit target (JSON pretty, INI,
  env); Feature-driven warnings (comments dropped, nesting/arrays flattened),
  `--strict` promotes to error; `-e` converts a subtree.

## Milestones (upcoming, in order)

Release infra is intentionally **last** — build the capability, then ship it.

- ⬜ **Small fixes.** New-key creation for INI (section-aware), object literals
  `{...}` and `.["key"]` bracket keys in the language, `-i.bak` backups,
  iterate-in-assignment (`.a[] = x`).
- 🚧 **M8 — YAML & TOML** (newly in scope). ✅ **TOML**: full lossless edit via
  `toml_edit` (query + edit + convert). ✅ **YAML**: query + convert (pure-Rust
  serde). ⬜ **YAML lossless in-place edit**: favored path is a greenfield
  pure-Rust `yaml-lib` CST (full ownership, no FFI/social/license baggage;
  grounded in the YAML 1.2.2 spec + yaml-test-suite) — `yqlib-sys` FFI is the
  fallback.
- ⬜ **Language polish.** Grow the builtin registry, regex (`test`/`match`), as
  real queries demand.
- ⬜ **Release infra — LAST.** Coverage (Coveralls), release workflow
  (cross-platform binaries + `publish-crates`), Homebrew/pkgx, man page,
  `--help` examples, shell completions.

## Format coverage

edikt's niche is *lossless editing of key-value / config formats*. This tracks
every popular one. **In scope** = a gap format nobody edits today without
clobbering layout (edikt's reason to exist). **Served** = already has a good
lossless editor; we don't rebuild it (we may still *read* it for conversion).
**Candidate** = plausible future addition, not yet committed.

| format | status | notes |
|---|---|---|
| JSONC / JSON5 | ✅ in scope (done) | the headline — `settings.json`, `tsconfig.json`, `devcontainer.json` |
| JSON | ✅ in scope (done) | read as a JSONC subset |
| INI | ✅ in scope (done) | `[section]` + `key = value`/`key : value`, inline comments |
| `.env` | ✅ in scope (done) | flat, line-level only, interpret nothing |
| `.properties` (Java) | ✅ in scope (done) | `key=value` / `key:value`; `\` continuations are a follow-up |
| flat `key = value` (`zoo.cfg`, `sysctl.conf`, `.npmrc`) | ✅ in scope (done) | handled by the env module (`-t env`) |
| TOML | ✅ in scope (done) | full lossless edit via `toml_edit` (comments, tables, layout) |
| YAML | ✅ query + convert | pure-Rust (serde); lossless in-place edit pending (greenfield `yaml-lib` favored) |
| XML (`.csproj`, `pom.xml`, `web.config`, `plist`) | 🟡 candidate | structured, high demand, heavier CST |
| HCL (Terraform) | 🟡 candidate | structured devops config |
| KDL | 🟡 candidate | newer config language |
| CSV / TSV | 🟡 candidate | tabular — a different edit model |

### INI dialects (one dialect-aware module)

The INI module (M4) should cover the INI-shaped formats people actually edit;
they differ in small ways (comment char, subsections, duplicate keys, quoting).
Capture each quirk as a fixture under `fixtures/ini/` as it comes up:

- Git config (`.gitconfig`, `.git/config`) — `[section "subsection"]`
- systemd units (`.service`, `.timer`, …) — duplicate keys allowed
- `.editorconfig`, `php.ini`, XDG desktop entries (`.desktop`)
- `pip.conf` / `pacman.conf` / `.gitmodules` / WireGuard `wg0.conf`

## Testing philosophy (applies to every milestone)

- **Roundtrip anything in our test space.** Every fixture under `fixtures/<fmt>/`
  must parse → serialize byte-identically. New edge cases become new fixtures.
- **Surgically touch any internal piece.** For each format, prove we can `set`/
  `del` any node and change *only* that node's bytes (one-line diffs).
- **Parity with the competition.** Prove we do the queries jq/yq do.
- **Anti-parity.** Prove we do what they *can't*: edit a commented `tsconfig`/
  `settings.json` without clobbering comments, layout, or trailing commas.
- **Library crates get first-class coverage**, fixture-driven where it fits.

## Conventions

- Crates live in `crates/`; fixtures in `fixtures/<format>/`.
- Shared dependencies go through `[workspace.dependencies]`; **crate versions are
  per-crate** so they can publish independently.
- CI is warnings-as-errors (`check`/`clippy`), `fmt --check`, and a cross-OS test
  matrix. Every change lands via branch → PR → green CI → squash-merge.
- **Apply formatting before committing** (`cargo fmt --all`, not just
  `--check`), and never gate on a *piped* check — `cargo fmt --check | tail &&
  echo ok` reports the pipe's exit status (0), not fmt's, which once masked
  unformatted code into CI. (`let`-chains format fine on rustfmt 1.9; the earlier
  "avoid them" note was a misdiagnosis of that masked check.)
