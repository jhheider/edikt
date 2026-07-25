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

## The moat (non-negotiable)

Every supported format has a **lossless CST**: parse -> tree that stores every
byte including whitespace and comments -> serialize is byte-identical. An edit
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
edikt -e EXPR [-e EXPR...] [FILE...]
edikt -f script.edk [-f ...] [FILE...]
```

| flag | meaning |
|---|---|
| *(positional EXPR)* | the expression, jq-style, when no `-e`/`-f` given |
| `-e, --expr EXPR` | inline expression; repeatable; applied in order |
| `-f, --file PATH` | read a script (statements, newline/`;` separated); repeatable; composes with `-e` in order. Scripts may open with **header directives** - `toFormat: FMT`, `type: FMT` - which CLI flags override; `#` header lines (comments, shebangs) are skipped |
| `-i, --in-place[=SUFFIX]` | write result back to each FILE; `-i.bak` keeps a backup (sed/perl style). Requires FILE; errors on stdin |
| `-t, --type FMT` | force **input** format (`jsonc`\|`json5`\|`json`\|`ini`\|`env`\|`properties`\|`toml`\|`yaml`) |
| `-T, --to FMT` | **output** format (default: the input format, preserved). `--json`/`--jsonc`/`--ini`/`--toml`/`--yaml` are shorthands for `-T <fmt>` |
| `-o, --output FILE` | write to FILE instead of stdout; queries/conversions infer the output format from FILE's extension (`-T` wins), mutations treat it as a sink. Nothing is written on a query miss |
| `-r, --raw` | force raw scalar output (default for scalars already) |

**Output-format precedence:** explicit CLI (`-T` / a `--fmt` shorthand) ->
`-o` FILE's extension -> script `toFormat:` directive -> the input format,
preserved. Input-format precedence:
`-t` -> script `type:` -> filename detection. `json` and `jsonc` are distinct
formats sharing one engine: JSON has no `Comments` capability, so JSONC -> JSON
is a real conversion that warns and drops comments.

**I/O defaulting (sed-shaped):** no FILE or `-` -> read stdin, write stdout.
FILE without `-i` -> read file, write result to stdout. `-i` -> write back per
file.

**Format detection:** extension first (`.jsonc`/`.json5`/`.json`,
`.ini`/`.cfg`/`.conf`, `.env`, `.properties`); `-t` overrides; **stdin without
`-t` and no reliable sniff -> error** (predictable beats magic). Content sniffing
is a later nicety, never the primary path.

---

## Expression language (jq-flavored, fuller v1)

Syntax family is jq/yq so nobody has to learn anything. Scope is capped: this is
an *edit* language, not a general-purpose one.

**Navigation**
- identity `.`
- field `.foo`, `.foo.bar`, `."quoted key"`, `.["key"]`
- **hyphenated bare keys on an assignment target**: `.dev-dependencies.serde = "1"`.
  Legal there and nowhere else, because an assignment LHS must be a path, so `-`
  cannot be subtraction. In a query it still is (`.total-length` subtracts the
  `length` builtin), so a query quotes the key and gets a diagnostic saying so.
  Joining requires the tokens to abut: `.a - b` is always an operator.
- index `.arr[0]`, `.arr[-1]`
- iterate `.arr[]`, `.obj[]`
- pipe `EXPR | EXPR`
- multi-output `.a, .b, .c`
- alternative `EXPR // EXPR` - the left's truthy outputs, else the right
  (a miss, `null`, or `false` falls back; a type *error* still propagates)
- comment `.foo.#` - the head comment of a node as a string; `.foo.#.head`
  / `.foo.#.inline` / `.foo.#.foot` pick a kind, `.#` is the document banner,
  `.items[].#` reads each element's. Terminal (nothing navigates past it); a
  missing comment is a miss. **Read and edit everywhere** (`.foo.# = "TODO"`,
  `.foo.# |= gsub(...)`, `del(.foo.#)`), across all seven formats; head/foot wrap
  to the file's width envelope, inline never wraps, and only the targeted
  comment's bytes change. The one boundary: a **compact/single-line** target
  (minified JSON, a YAML flow `[...]`) has no own line to hang an own-line comment,
  so it **errors cleanly** ("needs layout expansion") rather than reflowing bytes
  the user didn't touch; auto-expansion is deferred, revisit-reactively (see
  [`docs/design/comments-as-first-class.md`](./docs/design/comments-as-first-class.md)).
- comment stream `comments` - a document-wide stream of `{path, kind, text}`
  records over every comment (query: `comments | select(.text | test("TODO")) |
  .path` = which keys carry a TODO); as a mutation target, `comments |= gsub(...)`
  bulk-edits every comment's text and `del(comments)` clears them all.
- filter `.items[] | select(.enabled == true)`

**Mutation**
- assign `PATH = <expr>` - the RHS is evaluated in the value calculus
- update-assign `PATH |= <expr>` - RHS sees the current value as `.`
- append `.arr += [<expr>]`
- delete `del(PATH)`

**Value calculus** (what makes "fuller" fuller: the evaluator computes, it
doesn't just place literals):
- JSON literals: `"s"`, `1`, `1.5`, `true`, `false`, `null`, `[...]`, `{...}`
- arithmetic on numbers: `+ - * / %`
- string concat with `+`
- a small function registry, jq-named: `length`, `keys`, `has`, `type`,
  `tostring`, `tonumber`, `ascii_upcase`, `ascii_downcase`, `ltrimstr`,
  `rtrimstr`, `startswith`, `endswith`, `split`, `join`, and the regex family
  `test`, `match`, `capture`, `sub`, `gsub` (args `;`-separated, jq-style;
  optional trailing flags from `g i x s m`; `match` yields jq's match objects
  with codepoint offsets, and no match is an empty stream, a miss). One
  deliberate divergence: jq splices captures into `sub` replacements by string
  interpolation, which this language doesn't have: replacements use `$1` /
  `$name` references instead (sed-flavored; `$$` is a literal `$`). Grow this
  list deliberately, never speculatively.

**Value semantics differ by format.** JSON-family values are typed. INI/`.env`
values are strings. So `.count + 1` on `.env` coerces on demand
(`tonumber`-like) to compute, then writes the computed result; string ops
(`ascii_upcase`) always work. Storage still preserves bytes verbatim -
"interpret nothing" governs how `.env` is *stored*, not a computation the user
explicitly asked for.

**Explicitly still out of scope in v1:** user-defined functions, `reduce`/`foreach`,
variable bindings (`as $x`), `if/then/else`, path expressions as first-class
values, module imports. If the language starts wanting these, that's
a v2 conversation, not scope creep.

---

## Modes & output contract

edikt is in exactly one mode per run, decided by the expression:

| mode | trigger | output |
|---|---|---|
| **mutation** | expression contains `=`/`\|=`/`+=`/`del()` | the **whole** document, byte-identical except touched nodes. Cannot combine with an output format - edit first, then convert |
| **query / convert** (one unified mode) | everything else | each result, rendered **in the output format** (explicit, or the input format preserved) |

**Query/convert output, "output follows the format":**
- **scalar** -> raw text (no quotes); an explicitly-requested JSON-family output
  JSON-encodes it instead.
- **structural, pure path, output = input** -> the **original source slice**
  (format-preserving get: exact bytes, comments, layout; YAML block collections
  dedented to the margin so the fragment stands alone).
- **structural, otherwise** (computed result, or output ≠ input) -> the value
  **emitted via the output format's emitter**. Layout is the emitter's own, but
  a **pure-path** selection carries its **comments** across (the uniform
  comment model; see conversion below); a synthesized value has none to carry.
  Lossy degradations warn (`--strict` promotes); a value the output format
  **cannot represent errors, naming the formats that can** (derived from
  `Feature` sets).
- multiple matches -> one per line (structural results may span lines).

**Exit codes (sed-shaped):** `0` success, including a query that matched
nothing, which is a **silent no-op** like sed with no matching address ·
`2` parse, syntax, or evaluation error. `--exit-status` opts into jq's `1`
on zero matches, for presence tests; `//` supplies in-expression defaults.

`-i` needs a mutating expression or an explicit output format.

---

## Per-format semantics

- **JSONC / JSON5 / JSON** - full typed model. `.json` is read by the JSONC
  parser (superset); it just has no comments to preserve. Highest-value target
  (`tsconfig.json`, `settings.json`, `devcontainer.json`).
- **INI** - paths are `.section.key`; sectionless preamble keys are top-level.
  Values are strings. No arrays/objects; an array index into INI is a clean
  error (exit 2). Iteration over a section's keys is allowed.
- **`.env` / `.properties`** - flat `.KEY`, string values, **line-level editing
  only, forever.** No grammar, no interpolation, no quoting semantics, no type
  coercion in storage. Set the bytes after the separator; preserve everything
  else. (There is no single `.env` grammar: docker-compose, dotenv libs, and
  shell `source` disagree, so "correctly" parsing it is a bottomless bug queue.
  We don't.)
- **KDL** - lossless via `kdl-rs` (format-preserving by design; the `toml_edit`
  of KDL). A KDL node carries positional **arguments**, `key=value`
  **properties**, *and* **children**, so the `Value` mapping is a fixed,
  documented convention:
  - a document / children block -> object, one entry per node name in
    first-appearance order; a name repeated at the same level -> **array** of
    the occurrences, in document order;
  - one node: children/props only -> object (props first, then children);
    exactly one argument and nothing else -> that **scalar**; several arguments
    only -> **array**; a bare node -> `null`;
  - a node mixing arguments with props/children -> object with the arguments
    under the reserved key **`"-"`** (one arg -> scalar, several -> array);
  - paths read as printed: `.keybinds.normal.bind[0].["-"]`. Arrays of arrays
    have no KDL spelling and error cleanly on emit.
  Edits are surgical (set an arg/prop, create a leaf node, delete, append new
  occurrences); replacing a whole node body wholesale is refused rather than
  reflowed, like YAML.

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
| JSONC / JSON5 | ● | ● | ● | ● | - |
| JSON | - | ● | ● | ● | - |
| TOML | ● | ● | ● | ● | - |
| YAML | ● | ● | ● | ● | - |
| KDL | ● | ● | ● | ● | - |
| INI | ● | - | - | - | ● |
| `.env` / `.properties` | ● | - | - | - | - |

The set is consulted in two places:

- **Edit time** - an operation needing a feature the format lacks fails cleanly
  instead of via ad-hoc per-format checks: `.arr[]` on INI -> *"INI has no
  arrays"*, exit 2.
- **Conversion** - see below.

## Format conversion (`-T`, data-model mode)

Cheap given the `Value` projection the language already needs, but honest:
**conversion re-emits; it is not format-preserving.** Layout is the target
emitter's own. **Comments, though, are carried** across via a **uniform comment
model**: a shared vocabulary of three kinds: *head* (own-line, before a node),
*inline* (trailing on the node's line), *foot* (own-line, after a container's
last node), held in `Commented` (a `Value` enriched with per-node comments).
Each format parses its comments *out* to the model (`Document::to_commented`)
and each emitter places them back in its own syntax (`//`, `;`, `#`): N-in +
N-out against one model, not N×N per pair. A kind the target's grammar can't
hold **remaps** to one it can, with a warning (env has no inline comments ->
own line); a target with no `Comments` feature at all (JSON) **drops** them,
with a warning. Comments ride **pure-path** selections; a computed result has
none to carry, so converting a commented source through one warns. `-T FMT`
(≠ input) parses -> `Value` (+ commented projection) -> applies the expression ->
**emits the target format**.

Feasibility is **derived from `Feature`, not a hardcoded lattice.** Compute the
features the *source document actually uses* (are there comments? nesting depth
> 1? arrays? non-string scalars?), subtract the target's `FEATURES`; each
remaining feature is a **warning** on stderr naming the degradation, then edikt
does the best-effort conversion:

| lost feature | degradation |
|---|---|
| Comments | carried (uniform model, re-delimited natively); dropped - warned - only for a `Comments`-less target (JSON) or a synthesized value |
| Nesting | flattened to dotted keys (`a.b.c = v`, the `java.util.Properties` convention; inverse un-flattens on the way in) |
| Arrays | indexed dotted keys (`a.0`, `a.1`) |
| TypedScalars | scalars stringified |

Warnings are **per-used-feature and document-level**: a JSONC file that happens
to have no comments and no nesting converts to INI silently, because nothing was
actually lost. Conversion **completes** with exit 0; `--strict` promotes any
lost-feature warning to an error (exit 2) for automation that must not degrade.
Never silently drop data that has no degradation path; that is always at least
a warning.

---

## Architecture (Rust)

Workspace; each format is an isolated module with no cross-coupling.

- **`edikt`** (bin) - clap CLI; I/O + `-i` orchestration; mode dispatch;
  format detection; output contract; exit codes.
- **`edikt-core`** (lib) - the `Value` model; the `Commented` model (a `Value`
  enriched with head/inline/foot comments, for comment-preserving conversion);
  the `Feature` enum; the **expression language** (its own `logos` lexer +
  Pratt parser + evaluator / value calculus / function registry); the
  **`Document` trait** (format-agnostic seam: resolve path -> node handle(s),
  read value/source-slice/commented projection, format-preserving replace,
  delete, append) and a **`Convert` trait** (`Value` ↔ per-format emitter).
- **`edikt-syntax`** (lib) - shared **rowan** substrate: green-tree helpers,
  generic lossless serialize (walk green tree -> concat token text), splice /
  structural-sharing edit utilities usable by any format's `SyntaxKind`.
- **`edikt-jsonc` / `edikt-ini` / `edikt-env`** - each = a `logos` lexer + a
  parser emitting a rowan tree over `edikt-syntax`, typed AST accessors, a static
  `FEATURES: &[Feature]`, and impls of `Document` + `Convert`.
- **`edikt-toml`** - `Document`/`Convert` over `toml_edit`'s decor-preserving
  DOM (edits keep comments/layout; no rowan needed; `toml_edit` is the CST).
- **`edikt-kdl`** - `Document`/`Convert` over `kdl-rs`'s format-preserving
  document (same pattern as TOML: the library is the CST; per-node `leading` /
  `before_terminator` decor carries the comment model).
- **`edikt-yaml`** - pure Rust over `libyaml-safer`. Not a rowan CST: one parse
  pass composes the event stream into a **span tree** (every scalar/collection's
  byte range) that doubles as the data model *and* the edit map. Edits are a byte
  splice over the original source (untouched bytes preserved verbatim); merge
  keys (`<<`) resolve in the value projection. Same `Document`/`Convert` seam, so
  the CLI dispatches over it identically to the rowan formats.

**Why rowan+logos:** lossless-by-construction CST, edit = structural-sharing
splice (untouched nodes are the *same* green nodes -> provably byte-identical),
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

---

## Out of scope (say no to these)

- Being jq - no general-purpose/functional language (see v1 scope list above).
- Formatting / linting / reflowing - ever, for regions not targeted.

Note: the brief originally scoped out YAML/TOML as "already served" by yq/
`toml_edit`. That decision was **revised**: the goal is now to bring the common
formats in-house for completeness and conversion. **TOML** is done (lossless, via
`toml_edit`). **YAML** is done too: lossless in-place edit + query + convert,
**pure Rust**, driven by `libyaml-safer` (a safe port of the reference parser,
zero transitive deps). One parse pass yields both the data model and byte-precise
marks; edits are a byte splice over the untouched source, so comments/layout
survive (see the `edikt-yaml` module note below). We still don't rebuild JSON's
plain query (jq owns that) or reflow/format anything.
