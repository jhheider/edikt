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
- ✅ **M8 — YAML & TOML** (newly in scope). ✅ **TOML**: full lossless edit via
  `toml_edit` (query + edit + convert). ✅ **YAML**: **lossless in-place edit** +
  query + convert, **pure Rust** via `libyaml-safer` (safe port of the reference
  parser, zero transitive deps). One parse pass → a span tree that is both the
  data model and the byte-splice edit map; set/`|=`/`+=`/`del`/new-key all
  preserve comments and layout; merge keys (`<<`) resolve in queries. Replaced
  the serde floor — the greenfield-CST / `yqlib-sys` paths are moot.
- ✅ **Output follows the format.** A structural query result is returned in the
  document's format: a **pure-path** result (`.services`) as the original
  **source slice** (exact bytes, comments, layout; YAML blocks dedented); a
  **synthesized** result (`keys`, `.a + .b`) emitted via the output format's
  emitter. Output format = `-T`/`--json`/`--jsonc`/`--ini`/`--toml`/`--yaml` →
  script `toFormat:` directive → input format preserved. An unrepresentable
  result errors naming capable formats (from `Feature` sets). `json` vs `jsonc`
  are now distinct formats (JSON lacks Comments). `-o FILE` writes to a file,
  inferring the output format from its extension (`-T` wins; mutations treat it
  as a sink; nothing is written on a query miss).
- ✅ **Comment-preserving conversion.** Comments carry across `-T` via a **uniform
  comment model** — `Commented` in `edikt-core` (a `Value` enriched with per-node
  head / inline / foot comments), `Document::to_commented` extraction in all six
  formats, and per-format commented emitters that place each kind natively
  (`//`, `;`, `#`), remap a kind the grammar can't hold (env inline → own line,
  warned), or drop for a `Comments`-less target (JSON, warned) — the same
  Feature-subtraction path as other degradations. N-in + N-out against one
  model, not N×N per pair. Comments ride pure-path selections (aligned 1:1 with
  the evaluator); synthesized results have none, and converting a commented
  source through one still warns. YAML emits by splicing comments into the
  libyaml output at span-tree positions, so comment-free output stays
  byte-identical.
- ✅ **Language polish.** The regex family — `test`, `match` (jq match objects,
  codepoint offsets, `g` streams every match), `capture` (named groups), `sub`/
  `gsub` (`$name` capture references — sed-flavored, since the language has no
  string interpolation) — plus `split` (literal 1-arg, regex 2-arg, jq's shape),
  `join`, `startswith`, `endswith`. Flags `g i x s m`; a bad regex or flag is a
  clean exit-2 error; a no-match `match`/`capture` is an empty stream (a miss).
  Driven by the `regex` crate. The registry still grows deliberately, never
  speculatively.
- ⬜ **Comments as first-class content** (design-first — see
  [`docs/design/comments-as-first-class.md`](./docs/design/comments-as-first-class.md)).
  Make comments **addressable and editable**, not just preserved/carried:
  query them (`.foo.#` → head comment, `.foo.#.inline`, a document-wide
  `comments` stream), attach/edit/delete them format-preservingly
  (`.foo.# = "TODO"`, `.foo.# |= gsub(...)`), and search a comment back to the
  key it annotates. Reuses the `Commented` model; the real cost is (a) teaching
  the evaluator to see comments — it runs over the comment-free `Value` today —
  (b) per-format comment *write-back* (none exists yet; `to_commented` is
  read-only), and (c) key-carrying iteration for comment→path. `#` is the terse
  90% surface (collision-free; head comment by default); comment *kinds* become
  a `Feature`-derived capability (`CommentKind { Head, Inline, Foot }`), so
  `.env` inline errors/remaps the same way conversion already does. An identity
  shift ("edit values, preserve comments" → "edit the document, comments
  included") — deliberately post-v0.1, sequenced query → mutate → bulk.
- ⏸️ **Per-format feature flags — deferred.** The whole binary is ~1.5 MB
  stripped for all seven formats; the per-format delta doesn't justify a Cargo
  feature matrix + CI combinatorics. Revisit reactively (e.g. an `edikt-lite`
  build) if a concrete consumer needs a thinner binary.
- ✅ **Release infra.** Coverage job (cargo-llvm-cov → Coveralls) on the test
  workflow; `release.yml` on a published GitHub Release — five-target binary
  matrix (linux x86_64/aarch64, macos x86_64/aarch64, windows x86_64) with the
  man page + bash/zsh/fish completions inside each unix archive, then
  `katyo/publish-crates` in dependency order; `homebrew-bump.yml` PRs
  jhheider/homebrew-tap. The binary is its own doc generator (hidden
  `--manpage` / `--completions SHELL` flags, for packagers); `--help` closes
  with examples. Crates are at 0.1.0, dual-licensed MIT OR Apache-2.0.

  **Release ceremony** (the remaining manual steps, in order):
  1. repo secrets: `CARGO_REGISTRY_TOKEN` (crates.io), `HOMEBREW_TAP_TOKEN`
     (PAT, `public_repo`+`workflow`); enable the repo in Coveralls.
  2. `gh release create v0.1.0 --generate-notes` — binaries, crates, and the
     tap bump all flow from that one event.
  3. after the release exists: seed the `edikt` formula in jhheider/homebrew-tap
     (the bump action only updates an existing formula) and open the pkgx
     pantry PR.

## Format coverage

edikt's niche is *lossless editing of key-value / config formats*. This tracks
every popular one. **In scope** = a format edikt can edit losslessly, and where
that lossless edit is its reason to exist. **Served** = already has a good
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
| YAML | ✅ in scope (done) | lossless edit + query + convert, pure Rust via `libyaml-safer`; merge keys (`<<`) resolve in queries |
| KDL | ✅ in scope (done) | lossless via `kdl-rs` (the `toml_edit` of KDL); zellij/niri configs. Args/props/children → `Value` per the convention in CLAUDE.md (`"-"` args key, repeats → arrays) |
| XML (`.csproj`, `pom.xml`, `web.config`) | 🟡 candidate — demand-gated | genuine unserved gap (xmlstarlet/yq can't round-trip losslessly), but the biggest CST yet: no `toml_edit`-analog in Rust, and attributes-vs-children need a design doc + new `Feature` variants. Scope to *data-XML*; XML-plist rides as a dialect if this lands |
| HCL (Terraform) | ⛔ served | `hcledit` already edits HCL losslessly; `terraform fmt` canonicalizes layout anyway; HCL values are *expressions* the `Value` model can't honestly project |
| plist | ⛔ out of scope | hand-edited plists ≈ only Xcode's `project.pbxproj`, whose emitter is a moving target (the `.env` bottomless-bug-queue trap). Only ever a thin XML dialect, never standalone |
| CSV / TSV | ⛔ served | tabular, not path-shaped; ~no comments/layout to preserve — the moat doesn't apply. Miller / qsv own it |

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
- **Query coverage.** Prove the query surface handles the common jq/yq-style
  navigations users reach for.
- **Lossless-edit proof.** Prove we can edit a commented `tsconfig`/
  `settings.json` and change only the targeted bytes — comments, layout, and
  trailing commas untouched.
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
