# edikt — build contract

`edikt` is a **lossless, format-preserving structured-config editor** for
**JSONC/JSON5**, **INI**, **TOML**, **YAML**, and **sectionless key-value** files
(`.env`, `.properties`, `zoo.cfg`-style).

It edits with a **jq-flavored expression language** and a **sed-flavored
execution model** (stream-first, `-i` in place, `-e`/`-f` scripts). The one
thing it never does is reflow what it didn't touch — comments, indentation,
quoting, and trailing commas in every untouched region survive byte-for-byte.

This file is the design contract. Read it before changing behavior. If you
change a rule here, change it here *first*.

---

## The moat (non-negotiable)

Every supported format has a **lossless CST**: parse → tree that stores every
byte including whitespace and comments → serialize is byte-identical. An edit
touches only the nodes it targets; every untouched region is re-emitted
byte-for-byte. This is the entire reason the tool exists. Guard it:

- No format-preserving edit may alter indentation, comma style, quote style,
  comment placement, or trailing newline of any region it did not target.
- `parse ∘ serialize == identity` is a hard invariant, tested per format.
- We do **not** format, lint, or normalize. taplo and prettier own that.

---

## CLI contract

```
edikt [EXPR] [FILE...]
edikt -e EXPR [-e EXPR…] [FILE...]
edikt -f script.edk [-f …] [FILE...]
```

| flag | meaning |
|---|---|
| *(positional EXPR)* | the expression, jq-style, when no `-e`/`-f` given |
| `-e, --expr EXPR` | inline expression; repeatable; applied in order |
| `-f, --file PATH` | read a script (statements, newline/`;` separated); repeatable; composes with `-e` in order |
| `-i, --in-place[=SUFFIX]` | write result back to each FILE; `-i.bak` keeps a backup (sed/perl style). Requires FILE; errors on stdin |
| `-t, --type FMT` | force **input** format (`jsonc`\|`json5`\|`json`\|`ini`\|`env`\|`properties`\|`toml`\|`yaml`) |
| `-T, --to FMT` | **output** format → conversion / data-model mode (see below) |
| `-r, --raw` | force raw scalar output (default for scalars already) |
| `--json` | force JSON-encoded output |

**I/O defaulting (sed-shaped):** no FILE or `-` → read stdin, write stdout.
FILE without `-i` → read file, write result to stdout. `-i` → write back per
file.

**Format detection:** extension first (`.jsonc`/`.json5`/`.json`,
`.ini`/`.cfg`/`.conf`, `.env`, `.properties`); `-t` overrides; **stdin without
`-t` and no reliable sniff → error** (predictable beats magic). Content sniffing
is a later nicety, never the primary path.

---

## Expression language (jq-flavored, fuller v1)

Syntax family is jq/yq so nobody has to learn anything. Scope is capped: this is
an *edit* language, not a general-purpose one.

**Navigation**
- identity `.`
- field `.foo`, `.foo.bar`, `."quoted key"`, `.["key"]`
- index `.arr[0]`, `.arr[-1]`
- iterate `.arr[]`, `.obj[]`
- pipe `EXPR | EXPR`
- multi-output `.a, .b, .c`
- filter `.items[] | select(.enabled == true)`

**Mutation**
- assign `PATH = <expr>` — the RHS is evaluated in the value calculus
- update-assign `PATH |= <expr>` — RHS sees the current value as `.`
- append `.arr += [<expr>]`
- delete `del(PATH)`

**Value calculus** (what makes "fuller" fuller — the evaluator computes, it
doesn't just place literals):
- JSON literals: `"s"`, `1`, `1.5`, `true`, `false`, `null`, `[…]`, `{…}`
- arithmetic on numbers: `+ - * / %`
- string concat with `+`
- a small function registry, jq-named: `length`, `ascii_upcase`,
  `ascii_downcase`, `ltrimstr`, `rtrimstr`, `test`/`match` (regex, later),
  `tonumber`, `tostring`, `keys`, `has`, `type`. Grow this list deliberately,
  never speculatively.

**Value semantics differ by format.** JSON-family values are typed. INI/`.env`
values are strings. So `.count + 1` on `.env` coerces on demand
(`tonumber`-like) to compute, then writes the computed result; string ops
(`ascii_upcase`) always work. Storage still preserves bytes verbatim —
"interpret nothing" governs how `.env` is *stored*, not a computation the user
explicitly asked for.

**Explicitly still out of scope in v1:** user-defined functions, `reduce`/`foreach`,
variable bindings (`as $x`), `//` alternative operator, path expressions as
first-class values, module imports. If the language starts wanting these, that's
a v2 conversation, not scope creep.

---

## Modes & output contract

edikt is in exactly one mode per run, decided by the expression + flags:

| mode | trigger | output |
|---|---|---|
| **query** | expression has no assignment/`del` and `-T` absent | the selected value(s), one per line |
| **mutation** | expression contains `=`/`\|=`/`+=`/`del()`, `-T` absent | the **whole** document, byte-identical except touched nodes |
| **convert** | `-T FMT` present and ≠ input format | parse → `Value` → apply expr → **emit target format** (data-model; trivia dropped) |

**Query output:** scalar → raw (no quotes); structural (object/array) → the
**original source slice** (format-preserving get) by default, `--json` to
normalize; multiple matches → one per line.

**Exit codes (grep/jq-shaped):** `0` success / ≥1 match · `1` query with zero
matches · `2` parse, syntax, or evaluation error.

`-i` is only meaningful in mutation and convert modes.

---

## Per-format semantics

- **JSONC / JSON5 / JSON** — full typed model. `.json` is read by the JSONC
  parser (superset); it just has no comments to preserve. Highest-value target
  (`tsconfig.json`, `settings.json`, `devcontainer.json`).
- **INI** — paths are `.section.key`; sectionless preamble keys are top-level.
  Values are strings. No arrays/objects; an array index into INI is a clean
  error (exit 2). Iteration over a section's keys is allowed.
- **`.env` / `.properties`** — flat `.KEY`, string values, **line-level editing
  only, forever.** No grammar, no interpolation, no quoting semantics, no type
  coercion in storage. Set the bytes after the separator; preserve everything
  else. (There is no single `.env` grammar — docker-compose, dotenv libs, and
  shell `source` disagree — so "correctly" parsing it is a bottomless bug queue.
  We don't.)

---

## Format capabilities (`Feature`)

Each format module declares a **static capability set** so behavior is
*derived*, not special-cased per format pair:

```rust
enum Feature { Comments, Nesting, Arrays, TypedScalars, Sections }
// each format module: const FEATURES: &[Feature];
```

| format | Comments | Nesting | Arrays | TypedScalars | Sections |
|---|:-:|:-:|:-:|:-:|:-:|
| JSONC / JSON5 | ● | ● | ● | ● | — |
| JSON | — | ● | ● | ● | — |
| TOML | ● | ● | ● | ● | — |
| YAML | ● | ● | ● | ● | — |
| INI | ● | — | — | — | ● |
| `.env` / `.properties` | ● | — | — | — | — |

The set is consulted in two places:

- **Edit time** — an operation needing a feature the format lacks fails cleanly
  instead of via ad-hoc per-format checks: `.arr[]` on INI → *"INI has no
  arrays"*, exit 2.
- **Conversion** — see below.

## Format conversion (`-T`, data-model mode)

Cheap given the `Value` projection the language already needs — but honest:
**conversion drops trivia; it is not format-preserving.** `-T FMT` (≠ input)
parses → `Value` → applies the expression → **emits the target format**.

Feasibility is **derived from `Feature`, not a hardcoded lattice.** Compute the
features the *source document actually uses* (are there comments? nesting depth
> 1? arrays? non-string scalars?), subtract the target's `FEATURES`; each
remaining feature is a **warning** on stderr naming the degradation, then edikt
does the best-effort conversion:

| lost feature | degradation |
|---|---|
| Comments | dropped |
| Nesting | flattened to dotted keys (`a.b.c = v`, the `java.util.Properties` convention; inverse un-flattens on the way in) |
| Arrays | indexed dotted keys (`a.0`, `a.1`) |
| TypedScalars | scalars stringified |

Warnings are **per-used-feature and document-level** — a JSONC file that happens
to have no comments and no nesting converts to INI silently, because nothing was
actually lost. Conversion **completes** with exit 0; `--strict` promotes any
lost-feature warning to an error (exit 2) for automation that must not degrade.
Never silently drop data that has no degradation path — that is always at least
a warning.

---

## Architecture (Rust)

Workspace; each format is an isolated module with no cross-coupling.

- **`edikt`** (bin) — clap CLI; I/O + `-i` orchestration; mode dispatch;
  format detection; output contract; exit codes.
- **`edikt-core`** (lib) — the `Value` model; the `Feature` enum; the
  **expression language** (its own `logos` lexer + Pratt parser + evaluator /
  value calculus / function registry); the **`Document` trait** (format-agnostic
  seam: resolve path → node handle(s), read value/source-slice, format-preserving
  replace, delete, append) and a **`Convert` trait** (`Value` ↔ per-format
  emitter).
- **`edikt-syntax`** (lib) — shared **rowan** substrate: green-tree helpers,
  generic lossless serialize (walk green tree → concat token text), splice /
  structural-sharing edit utilities usable by any format's `SyntaxKind`.
- **`edikt-jsonc` / `edikt-ini` / `edikt-env`** — each = a `logos` lexer + a
  parser emitting a rowan tree over `edikt-syntax`, typed AST accessors, a static
  `FEATURES: &[Feature]`, and impls of `Document` + `Convert`.
- **`edikt-toml`** — `Document`/`Convert` over `toml_edit`'s decor-preserving
  DOM (edits keep comments/layout; no rowan needed — `toml_edit` is the CST).
- **`edikt-yaml`** — pure Rust over `libyaml-safer`. Not a rowan CST: one parse
  pass composes the event stream into a **span tree** (every scalar/collection's
  byte range) that doubles as the data model *and* the edit map. Edits are a byte
  splice over the original source (untouched bytes preserved verbatim); merge
  keys (`<<`) resolve in the value projection. Same `Document`/`Convert` seam, so
  the CLI dispatches over it identically to the rowan formats.

**Why rowan+logos:** lossless-by-construction CST, edit = structural-sharing
splice (untouched nodes are the *same* green nodes → provably byte-identical),
one framework across all formats. This is the taplo/rust-analyzer pattern; it
makes "both formats at once" cheaper because they share the harness.

The expression evaluator is written and tested against a plain in-memory `Value`
model **first**, then wired to the CSTs. Language correctness and CST fidelity
de-risk independently.

---

## Step zero: the CST-fidelity spike (gating)

**Status: ✅ VALIDATED (2026-07-04).** The `spike/` crate (git-ignored) stands up
a minimal `rowan`+`logos` JSONC lexer/parser and proves, on a gnarly 274-byte
`tsconfig` (tab indent, trailing commas, `//` + `/* */` comments, nested
objects/arrays): byte-identical round-trip; a deep one-node edit changing exactly
one line; every untouched region byte-for-byte identical. INI round-trip + a
spacing-preserving value edit also pass. The `rowan` structural-sharing splice is
the mechanism — untouched nodes are the *same* green nodes, so byte-identity is
guaranteed by construction, not bookkeeping. **Build proceeds.**

The gating question this answered: prove the tree does lossless **edits**, not
just lossless parse. This is the fork between a weekend-per-format and a
fortnight-per-format; we now know it's the weekend side.

1. Stand up a minimal `rowan`+`logos` JSONC lexer/parser (objects, arrays,
   scalars, `//` and `/* */` comments, whitespace as trivia).
2. **Round-trip:** parse a gnarly commented `tsconfig.json` (nested, trailing
   commas, mixed indent, both comment styles) → serialize → **byte-identical**.
3. **One-node edit:** locate one deep value, splice a new value node, serialize
   → **exactly one region differs**.
4. Repeat the round-trip probe for INI.
5. Benchmark `jsonc-parser` (dprint/David Sherret) as a baseline so the
   rowan choice is evidenced.

Pass → build. If rowan editing proves ergonomically wrong for our splice
pattern, reconsider before writing more.

---

## Milestones

Live status and the full backlog live in [`ROADMAP.md`](./ROADMAP.md). In brief:

- ✅ **M0** Spike → CST decision: rowan+logos, lossless edit proven.
- ✅ **M1** Skeleton + **query mode on JSONC** end-to-end (workspace, CLI,
  `Value`/expression language/evaluator, lossless CST + `Document` seam, output
  contract + exit codes).
- ✅ **M2** Mutation on JSONC: `=`, `|=`, `+=`, `del()` + the format-preserving CST
  **write** path (rowan splice) + `-i`. *The differentiator.*
- ✅ **M4** INI. ✅ **M5** `.env`/`.properties`. ✅ **M6** conversion (`-T`,
  Feature-driven warnings).
- ✅ **M8** TOML (lossless via `toml_edit`) and YAML (lossless via pure-Rust
  `libyaml-safer` span-tree splice) — edit + query + convert.
- **M3** builtin/query polish and **M7** release infra remain (release infra is
  intentionally last — see ROADMAP).

Realistic effort with fuller language + both formats + conversion: **3–5 weeks
part-time.** The language is the one thing that can balloon it — hold the v1
scope line.

---

## Testing

- **Round-trip corpus** per format: `parse ∘ serialize ≡ input`, byte-identical,
  over a fixture set of real-world gnarly files.
- **Edit fixtures**: `(input, expr) → expected_output`, asserting minimal diffs
  (a set should change exactly the targeted line).
- **Language tests**: the evaluator against the in-memory `Value` model,
  independent of any CST.
- **Conversion tests**: feasibility lattice, including the error cases.
- Property test the identity invariant (`parse∘serialize`) where feasible.

---

## CI (house pattern)

Model on `jhheider/gpg-inspector` and `rpghearth/hearth-app` (good-netizen Rust
CI). Workflows to mirror:

- **check-and-lint** — `cargo check --workspace --all-features` with
  `RUSTFLAGS: -D warnings`; `cargo fmt --all -- --check`; `cargo clippy
  --workspace --all-targets --all-features` with `-D warnings`. `paths-ignore`
  docs; `concurrency` cancel-in-progress; `Swatinem/rust-cache`;
  `dtolnay/rust-toolchain@stable`.
- **test** — matrix over ubuntu/macos/windows: `cargo test --workspace
  --all-features`; plus a coverage job (`cargo-llvm-cov` → lcov → Coveralls).
- **audit** — `rustsec/audit-check` on a weekly cron + `workflow_dispatch`.
- **release** — on published GitHub Release: cross-platform binary matrix
  (linux x86_64/aarch64, macos x86_64/aarch64, windows x86_64) attached to the
  release, then `katyo/publish-crates` publishing workspace crates in dependency
  order.

Warnings are errors in CI, so the format-preservation invariants and the
round-trip corpus must be green before merge.

**Loop discipline & gotchas:**
- Every change lands via **branch → PR → green CI → squash-merge**; never commit
  straight to `main`.
- **Apply formatting before committing** with `cargo fmt --all` (not just
  `--check`), and never gate on a *piped* check: `cargo fmt --check | tail && echo
  ok` reports the pipe's exit status (0), not fmt's — that masked unformatted
  code into CI once. (`let`-chains format fine on rustfmt 1.9; the earlier
  "avoid them" note was a misdiagnosis of that masked check.)
- Crates live in `crates/`; **fixtures in `fixtures/<format>/`** and every one
  must round-trip byte-identically. Shared deps go through
  `[workspace.dependencies]`; crate **versions are per-crate**.

---

## Out of scope (say no to these)

- Being jq — no general-purpose/functional language (see v1 scope list above).
- Formatting / linting / reflowing — ever, for regions not targeted.

Note: the brief originally scoped out YAML/TOML as "already served" by yq/
`toml_edit`. That decision was **revised** — the goal is now to bring the common
formats in-house for completeness and conversion. **TOML** is done (lossless, via
`toml_edit`). **YAML** is done too — lossless in-place edit + query + convert,
**pure Rust**, driven by `libyaml-safer` (a safe port of the reference parser,
zero transitive deps). One parse pass yields both the data model and byte-precise
marks; edits are a byte splice over the untouched source, so comments/layout
survive (see the `edikt-yaml` module note below). We still don't rebuild JSON's
plain query (jq owns that) or reflow/format anything.
