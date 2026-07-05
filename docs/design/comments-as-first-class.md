# Design: comments as first-class content

**Status:** accepted design for **v0.2.0** (design-first; no code yet). Builds on
the "comment-preserving conversion" work by *reusing its model* (`Commented`) —
this makes comments addressable and editable, not just preserved and carried.
Core decisions are settled (see [Decided](#decided-was-open)); two sub-questions
remain for the implementation phases.

## The identity shift (name it up front)

edikt today is **"edit values, preserve comments."** This proposal makes it
**"edit the document, comments included."** That is a real widening of scope —
the same kind of move as adding `//` to the language, just further. It is worth
doing because it deepens the moat rather than diluting it: *losslessly querying
and bulk-editing the comments themselves, addressably,* is something no config
tool does (yq preserves some comments; none treat them as searchable, editable
content). It stays inside the "lossless, surgical, never reflow what you didn't
touch" philosophy — a comment edit changes only that comment's bytes, or inserts
exactly one comment line.

## Motivating use cases

1. **Iterate all comments, replace `foo` with `bar`.** Bulk edit of comment
   *text* across the document.
2. **Find all values matching X, add a comment** (TODO / deprecation / warning).
   A value predicate that attaches a comment.
3. **Add a value with a comment** — create a key and annotate it in one pass.
4. **Search for a comment, return its attached key.** A comment predicate that
   yields the *path* it annotates.

All four are one capability: **comments become an addressable dimension of the
document**, for both query and mutation.

## The model already exists

`edikt-core`'s `Commented` (a `Value` enriched with per-node `head` / `inline` /
`foot` comments) is exactly the model this needs. What is missing is the two
ends:

- a way to **name** comments in the expression language (the evaluator today
  runs over the comment-free `Value`; trivia is gone before it looks), and
- a way to **write comments back** into a live CST (the seven `to_commented`
  impls are read-only extraction; nothing splices a comment back today).

## Addressing surface

`#` is the sigil for the comment dimension. It cannot collide with a real key
(`#` is not a valid identifier start), which is the whole reason to prefer it
over a synthetic `.comment` key.

### The 90% case — `#` alone is the head comment

```
.foo.#                 # the head comment of .foo (own-line, above it)
.#                     # the document's leading (banner) comment
.foo.# = "TODO: drop"  # attach/replace the head comment on .foo
del(.foo.#)            # remove .foo's head comment
```

`#` bare resolves to the **head** comment because own-line annotations
(TODO/deprecation/section banners) are the dominant real-world case.

### The other 10% — the `#` namespace by kind

`#` is really a synthetic sub-node exposing the `Commented` record; the bare form
is sugar for its `head` field.

```
.foo.#.head            # ≡ .foo.#   (the shorthand)
.foo.#.inline          # the trailing comment on .foo's line
.foo.#.foot            # own-line comment(s) after .foo
.foo.#                 # sugar for .foo.#.head
```

- `head`, `inline`, and `foot` are each a **single string** (the comment text,
  delimiter-free). A multi-line head/foot comment is *one* string with the
  emitter's wrapping applied — **not** an array of lines (decided: rational
  wrapping over arrays; see [Wrapping](#wrapping-long-comments)). Reading a
  multi-line head comment returns the unwrapped text (lines joined to a space);
  writing wraps it back.
- Reading a missing comment is a **miss** (empty stream) — sed-shaped, so
  `.foo.# // "no note"` supplies a default and `del` of an absent comment is a
  no-op, consistent with the rest of the language.

*(Rejected alternative: a real `.foo.comment` node or `.foo.comment[head]`
index. It collides with a legitimately-named `comment`/`comments` key, and the
`[head]` bare-word index is not in the path grammar. The `#` namespace is
collision-free and composes with existing field syntax.)*

### Document-wide comment stream (enables cases 1 and 4)

Bulk operations need every comment *with the path it annotates*. A top-level
`comments` accessor streams one record per comment:

```
comments                       # stream of { path, kind, text } over the whole doc
comments | select(.text | test("TODO"))          # case 4 (partial): find them…
comments | select(.text | test("TODO")) | .path  # …return their keys
```

`.path` is a rendered path string (`.a.b.#.inline`) — this is the one place the
language must **carry keys through iteration**, which it deliberately does not do
today (`.[]` discards keys). That dependency is called out under Cost.

## Mutation semantics

```
.foo.# = "deprecated"                     # attach a head comment (case 3, with `.foo = 1 | …`)
.foo = 1 | .foo.# = "why"                 # add a value AND annotate it, one pipe (case 3)
(.[] | select(. == 42)) as targets …      # case 2 needs value→path; see Cost
.foo.# |= gsub("foo"; "bar")              # edit one comment's text
comments |= gsub("foo"; "bar")            # case 1: bulk edit ALL comment text
del(.foo.#.inline)                        # remove just the inline comment
```

Every write is **format-preserving**: setting an existing comment changes only
its bytes; attaching a new one inserts exactly one comment line (or trailing
segment, for inline), touching nothing else — the same surgical guarantee as
value edits. The one exception is when the *layout can't hold* an own-line
comment; see [Layout reflow](#layout-reflow-when-a-comment-forces-expansion).

## The `Feature` model: comment kinds are the feature (the load-bearing part)

Comment *kinds are not uniform across formats*, so which kinds a format supports
is the capability — exactly the "features section matters again" point. Per the
decision, **the comment feature *is* an array**: a format declares

```rust
enum CommentKind { Head, Inline, Foot }
const COMMENT_KINDS: &[CommentKind];   // empty ⇒ no comment support at all
```

and `COMMENT_KINDS` **subsumes** the old boolean `Feature::Comments`: "has
comments" is just `!COMMENT_KINDS.is_empty()`, so JSON (`&[]`) needs no special
case. It is the single source of truth, consulted in **both** places so the
logic is derived, never special-cased:

- **edit time** — `.KEY.#.inline = "x"` on `.env` fails cleanly
  (*"`.env` has no inline comments"*), the same way `.arr[]` on INI fails for
  arrays; a comment on JSON fails (*"JSON has no comments; use jsonc"*).
- **conversion time** — this *is* the remap rule the commented-emit path already
  implements ad hoc (env inline → own line, warned). Unifying it here replaces
  per-emitter special cases with one derived check (target lacks the kind →
  remap to a kind it has, or drop, with a warning).

Comment-kind support by format (`COMMENT_KINDS` contents):

| format | head | inline | foot |
|---|:-:|:-:|:-:|
| JSONC / JSON5 | ● | ● | ● |
| JSON | — | — | — |
| TOML | ● | ● | ● |
| YAML | ● | ● | ● |
| KDL | ● | ● | ● |
| INI | ● | ● | ● |
| `.env` / `.properties` | ● | — | ● |

Inline is the only kind with a real per-format gap (`.env` — a `#` inside a
value is data, not a comment). JSON supports none.

## Layout reflow: when a comment forces expansion

The moat says "never reflow what you didn't touch," and a comment edit honors it
— *except* where the target layout has no line to hang an own-line comment on.
Attaching one then forces the **minimal enclosing structure** to expand, which is
a real reflow of bytes the user didn't target, so it **warns** (and `--strict`
promotes to an error). The cases:

- **Compact / single-line JSONC** — `{"a":1}` has no line structure; adding
  `.a.#` (a head comment) forces the object to multi-line (pretty) so the comment
  has a line above `"a"`. Warn: *"adding a comment expanded a compact object to
  multi-line"*.
- **YAML flow collections** — `key: [a, b]` can't carry an own-line comment on an
  element; `.key[0].#` forces flow → block style. Warn: *"adding a comment
  converted a flow sequence to block style"*.
- **Inline into a flow/compact element** — an *inline* comment has nowhere legal
  to sit inside `[a, b]`; this errors rather than reflows (there is no non-lossy
  placement), naming the head form as the alternative.

These are the only comment edits that touch more than their own line; every
other attach/edit/delete stays surgical. A warning here matters because the
whole promise is byte-minimal diffs — a silent whole-object reflow would betray
it.

## Wrapping long comments

Comment text is stored **unwrapped** (one logical string); the emitter wraps on
write. Decided: **rational wrapping (and re-wrapping), not line arrays** — and
the width is *the file's own envelope*, so comments never make the document
wider than it already is.

- **Wrap column (absolute)** `= min(longest line in the source, 100)`. The file
  is already this wide; a wrapped comment that fits inside it adds no horizontal
  extent — the moat, applied to columns. The `100` is a hard ceiling so a
  pathologically wide file doesn't yield 200-char comment lines. The longest
  line is measured on the **original source** (before the edit), so edits don't
  ratchet the width up.
- **Text budget at a comment's indent** `I` with delimiter `D` (`"# "`, `"// "`)
  `= wrap_col − I − len(D)`. This is what "longest line minus current indent"
  computes, anchored to the absolute edge so a deeply-indented comment still
  ends *at or before* `wrap_col` — never past it. Continuation lines repeat `D`
  at column `I`.
- **No lower floor.** A genuinely narrow file (longest line 40) keeps its
  comments at 40 — that is the point ("use the space as it exists"). A floor
  could push a comment past the longest line and *expand* the envelope, which is
  the one thing this rule exists to prevent.
- **Unbreakable tokens overflow** rather than hard-break (a URL or path longer
  than the budget). So "no expansion" is best-effort: we never widen *by choice*,
  only when a single word leaves no option.
- **Empty / comment-free source** has no longest line to learn from → fall back
  to the `100` ceiling.
- **Inline comments never wrap** — a wrapped trailing comment would spill onto a
  line that reads as the *next* node's head comment. A long inline stays long
  (or the author should have used head).
- **Re-wrapping on edit** — `.foo.# |= gsub(...)` re-wraps the result to the same
  column. This reflows the comment being edited (the thing you targeted, so
  fine) but never touches neighboring comments.

## The write interface (not a new representation)

We are **not** missing a unified AST/CST — the heterogeneity (rowan for
JSONC/INI/env, `toml_edit`, `kdl-rs`, span-tree for YAML) is deliberate: each
format reuses the best-in-class lossless library, and forcing them onto one tree
would throw away `toml_edit`'s correctness and require building a `yaml_edit`
that does not exist. `Commented` is already the unified *read/convert* model.

What's missing is a unified **write** interface — new methods on the `Document`
trait, each format implementing over its own substrate:

```rust
fn set_comment(&mut self, path: &[Step], kind: CommentKind, text: &str) -> Result<(), EditError>;
fn delete_comment(&mut self, path: &[Step], kind: CommentKind) -> Result<(), EditError>;
// read side already exists as to_commented(); a `comments()` iterator is Phase 3
```

The language/CLI dispatch through these uniformly; each format splices into its
own tree. Unify the interface, not the representation.

### On a `yaml_edit` layer — deliberately not building it

Attaching a head comment to YAML is "insert `<indent># text\n` before the
node's line," and the span tree already carries the node's byte offset — so it
reuses the *same byte-splice* the existing new-key/append edits use (days on
existing machinery). A full `yaml_edit` decor-CST (mirroring `toml_edit`) would
mean modeling decor slots for every YAML construct by reconstructing inter-token
trivia from libyaml's events — weeks, high risk, duplicating libyaml. The
span-tree splice suffices; don't build the layer.

## Cost, in tiers (honest)

1. **Shallow — write-back for TOML & KDL.** Comments are editable decor strings;
   the commented-emit path already writes them.
2. **Mechanical — write-back for JSONC / INI / `.env`.** Insert or replace a
   comment token in the rowan tree (they already carry comment tokens; the splice
   utilities exist for values).
3. **Moderate — write-back for YAML.** A byte-splice at a computed offset over
   the span tree — the same mechanism as value edits, *not* a new `yaml_edit`
   layer (see above). The layout-reflow cases (flow → block) are the fiddly part.
4. **Deep — the evaluator must see comments.** `eval` runs over `Value`, which is
   comment-free. Addressing comments needs eval (or a parallel resolve path) to
   run over `Commented`, or a dedicated comment-resolution pass the `#` namespace
   dispatches into. The architectural core, not a builtin.
5. **Deep — key-carrying iteration** for `comments`' `.path` (case 4) and
   value→path for "annotate matching values" (case 2). The language discards keys
   on iteration today; this needs a `path`-aware primitive (jq's
   `path`/`to_entries` family), itself a scope expansion.

## Recommended scope & sequencing — **v0.2.0**

Confirmed as the **0.2.0** milestone (ships after v0.1.0; not a blocker for it).

- **Phase 1 — comment query (read-only).** The `#` namespace for reading
  (`.foo.#` → head, `.foo.#.inline`/`.foot`), missing = miss, plus the
  `COMMENT_KINDS` capability. Reuses `Commented`; no write paths. De-risks the
  addressing surface cheaply, on all formats at once.
- **Phase 2 — comment mutation.** The `Document` write methods, easy formats
  first (TOML/KDL → JSONC/INI/env → YAML). Attach / edit-text / delete for
  `head` and `inline`; wrapping + re-wrapping; the layout-reflow warnings.
- **Phase 3 — document-wide `comments` + bulk edit + comment→key.** Requires the
  key-carrying-iteration primitive; the largest and least-certain piece, so it
  goes last and its shape is confirmed by what Phase 1/2 usage actually demands.

## Decided (was "open")

- **`#` defaults to the head comment.** ✅
- **Bulk edit (`comments |= …`) is in scope** (Phase 3). ✅
- **`del(.foo)` takes attached comments with the node** — no "keep the comment"
  escape. ✅
- **Multi-line comments are wrapped strings, not line arrays** — wrapped to the
  file's own envelope (`min(longest source line, 100)`) so a comment never
  widens the document; re-wrapped on edit (see
  [Wrapping](#wrapping-long-comments)). ✅

## Still open

- **Bulk `comments |= f` binding** — how an update-assign over a *stream of
  comment records* maps each result back to its node (the mutation engine must
  resolve `.path` per record); spec in Phase 3.

## Out of scope (for this milestone)

- Synthesizing *layout* (blank lines, alignment) — comments only.
- Comment *migration* on structural moves (a moved value does not carry its
  comment; a deleted one drops it).
- Recursive descent (`..`) as a general operator — if bulk needs it, add the
  narrow `comments` stream, not full `..`.
