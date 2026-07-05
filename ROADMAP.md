# edikt roadmap

Backlog and direction. The build contract (invariants, CLI, architecture) lives
in [`CLAUDE.md`](./CLAUDE.md); this file is the sequencing.

## Status

- ✅ **M0** — CST-fidelity spike (rowan+logos lossless edit proven).
- ✅ **M1** — query mode on JSONC end-to-end: workspace + CI, `edikt-core`
  (Value, Feature, expression language, evaluator), `edikt-syntax` + `edikt-jsonc`
  (lossless CST, `Document` seam), and the `edikt` CLI.

## Milestones

- ⬜ **M2 — Mutation + the format-preserving write path.** `set`/`del`/`+=`/`|=`
  grammar; mutation evaluation; the rowan structural-sharing splice for JSONC
  (edit only the targeted node, everything else byte-identical); mutation-mode
  whole-document output; wire `-i` (and `-i.bak`). This is the differentiator.
- ⬜ **M3 — `select()` / iteration polish + more builtins** as real queries drive
  demand. (Iteration and `select` already work; grow the builtin registry
  deliberately.)
- ⬜ **M4 — INI format module.** Line-oriented lossless CST; `.section.key`
  paths; `FEATURES = [Comments, Sections]`.
- ⬜ **M5 — `.env` / `.properties`.** Flat, string-valued, honest line-level
  editing only. `FEATURES = [Comments]`.
- ⬜ **M6 — Format conversion (`-T`).** Feature-driven: derive the source's used
  features, subtract the target's `FEATURES`, warn (or `--strict` error) per lost
  feature, degrade per the table in `CLAUDE.md`.
- ⬜ **M7 — Release infra.** Coverage job (Coveralls), release workflow
  (cross-platform binaries + `publish-crates`), man page, `--help` polish.

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
