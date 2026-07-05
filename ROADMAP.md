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

## Milestones

- ✅ **M2 — Mutation + the format-preserving write path.** `set` (`=`, `|=`),
  `del()`, `+=`/append, `-i`. Follow-ups: `-i.bak`, iterate-in-assignment
  (`.a[] = x`), object literals `{...}`, new-key creation.
- ⬜ **M3 — `select()` / iteration polish + more builtins** as real queries drive
  demand. (Iteration and `select` already work; grow the builtin registry
  deliberately.)
- ✅ **M4 — INI format module.** Line-oriented lossless CST; `.section.key`
  paths; inline-comment aware; `FEATURES = [Comments, Sections]`. Dialect quirks
  (git config subsections, systemd duplicate keys) are follow-ups.
- ⬜ **M5 — `.env` / `.properties`.** Flat, string-valued, honest line-level
  editing only. `FEATURES = [Comments]`.
- ⬜ **M6 — Format conversion (`-T`).** Feature-driven: derive the source's used
  features, subtract the target's `FEATURES`, warn (or `--strict` error) per lost
  feature, degrade per the table in `CLAUDE.md`.
- ⬜ **M7 — Release infra.** Coverage job (Coveralls), release workflow
  (cross-platform binaries + `publish-crates`), man page, `--help` polish.

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
| `.env` | ⬜ in scope (M5) | flat, line-level only, interpret nothing |
| `.properties` (Java) | ⬜ in scope (M5) | `key=value` / `key:value`, `\` line-continuations |
| flat `key = value` (`zoo.cfg`, `sysctl.conf`, `.npmrc`, `.editorconfig`-body) | ⬜ in scope (M5) | env/properties family |
| TOML | 🔵 served (`toml_edit`, yq) | KV, but already lossless-editable |
| YAML | 🔵 served (yq) | comments + mostly layout |
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
