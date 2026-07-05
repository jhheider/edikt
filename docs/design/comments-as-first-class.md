# Design: comments as first-class content

**Status:** proposal (design-first, no code). Supersedes the "comment-preserving
conversion" work by *reusing its model* — this makes comments addressable and
editable, not just preserved and carried.

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

- `head` and `foot` are **arrays of lines** (a block comment or a run of `//`
  lines is multi-entry); assigning a string sets a single line, assigning an
  array sets several.
- `inline` is a **single string or null**.
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
its bytes; attaching a new one inserts exactly one comment token/line at the
node's head (or after it, for inline/foot), touching nothing else — the same
surgical guarantee as value edits.

## The `Feature` model gets richer (the load-bearing part)

Comment *kinds are not uniform across formats*, so which kinds a format supports
becomes a first-class capability — exactly the "features section matters again"
point. Two shapes considered:

- **(a)** split `Feature::Comments` into `HeadComments` / `InlineComments` /
  `FootComments`. Simple, but foot is niche and this bloats the conversion
  lattice.
- **(b) recommended:** keep `Feature::Comments` as "has any comments" and add a
  per-format `comment_kinds() -> &[CommentKind]` where
  `enum CommentKind { Head, Inline, Foot }`. Consulted in **both** places, so
  the logic is derived, never special-cased:
  - **edit time** — `.KEY.#.inline = "x"` on `.env` fails cleanly
    (*"`.env` has no inline comments"*), the same way `.arr[]` on INI fails for
    arrays;
  - **conversion time** — this *is* the remap rule the commented-emit path
    already implements ad hoc (env inline → own line, warned). Unifying it here
    replaces per-emitter special cases with one derived check.

Comment-kind support by format:

| format | head | inline | foot |
|---|:-:|:-:|:-:|
| JSONC / JSON5 | ● | ● | ● |
| JSON | — | — | — |
| TOML | ● | ● | ● |
| YAML | ● | ● | ● |
| KDL | ● | ● | ● |
| INI | ● | ● | ● |
| `.env` / `.properties` | ● | — | ● |

Setting *any* comment on JSON errors (*"JSON has no comments; use jsonc"*).
Inline is the only kind with a real per-format gap (`.env`).

## Cost, in tiers (honest)

1. **Shallow — comment write-back for TOML & KDL.** Comments are editable decor
   strings; the commented-emit path already writes them. Splicing into a live
   tree is a small step from there.
2. **Mechanical — write-back for JSONC / INI / `.env`.** Insert or replace a
   comment token in the rowan tree (these already carry comment tokens; the
   splice utilities exist for values).
3. **Fiddly — write-back for YAML.** Comments are not span-tree nodes; they live
   in raw bytes between spans. Attaching one is a byte-splice at a computed
   offset, like the value edits — the hardest emitter, as with conversion.
4. **Deep — the evaluator must see comments.** `eval` runs over `Value`, which
   is comment-free. Addressing comments needs eval (or a parallel resolve path)
   to run over `Commented`, or a dedicated comment-resolution pass the `#`
   namespace dispatches into. This is the architectural core, not a builtin.
5. **Deep — key-carrying iteration** for `comments`' `.path` (case 4) and
   value→path for "annotate matching values" (case 2). The language discards
   keys on iteration today; this needs a `path`-aware primitive (jq's
   `path`/`to_entries` family), itself a scope expansion.

## Recommended scope & sequencing

- **v0.1.0 ships without this.** It is a milestone, not a blocker.
- **Phase 1 — comment query (read-only).** The `#` namespace for reading
  (`.foo.#`, `.foo.#.inline`), missing = miss, plus the `Feature`/`CommentKind`
  refinement. Reuses `Commented`; no write paths. De-risks the addressing surface
  cheaply, on all formats at once.
- **Phase 2 — comment mutation.** Write-back per format, easy formats first
  (TOML/KDL → JSONC/INI/env → YAML). Attach / edit-text / delete for `head` and
  `inline`.
- **Phase 3 — document-wide `comments` + bulk edit + comment→key.** Requires the
  key-carrying-iteration primitive; the largest and least-certain piece, so it
  goes last and its shape is confirmed by what Phase 1/2 usage actually demands.

## Out of scope (for this milestone)

- Synthesizing *layout* (blank lines, alignment) — comments only.
- Comment *migration* on structural moves (deleting a key deletes its attached
  comments; it does not relocate them).
- Recursive descent (`..`) as a general operator — if bulk needs it, add the
  narrow `comments` stream, not full `..`.

## Open questions

1. **`#` default = head or inline?** Proposed head (annotations sit above). If
   real use leans inline, flip the sugar — cheap before release, not after.
2. **Bulk `comments |= f` binding.** How does an update-assign over a *stream of
   comment records* map each result back to its node? Needs the mutation engine
   to resolve `.path` per record — spec this in Phase 3.
3. **`del(.foo)` and attached comments.** Confirmed: deletes them with the node
   (they annotate it). Should there be a "keep the comment, drop the value"
   escape? Probably not v1.
4. **Multi-line `head`/`foot` as arrays** vs. a single `\n`-joined string. Arrays
   proposed (matches `Commented`); revisit if it feels heavy in practice.
