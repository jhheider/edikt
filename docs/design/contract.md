# edikt design contract

`edikt` is a **lossless, format-preserving structured-config editor** for
**JSONC/JSON5**, **INI**, **TOML**, **YAML**, **KDL**, and **sectionless
key-value** files (`.env`, `.properties`, `zoo.cfg`-style), plus the
frontmatter block of Markdown files and PEP 723 script headers.

This is the contract for its behavior. Read it before changing behavior, and
if a rule changes, change it here *first*. Status and backlog live in
[`ROADMAP.md`](../../ROADMAP.md).

---

## The moat (non-negotiable)

Every supported format has a **lossless CST**: parse -> tree that stores every
byte including whitespace and comments -> serialize is byte-identical. An edit
touches only the nodes it targets; every untouched region is re-emitted
byte-for-byte. This is the entire reason the tool exists. Guard it:

- No format-preserving edit may alter indentation, line endings, comma style,
  quote style, comment placement, or trailing newline of any region it did
  not target.
- `parse ∘ serialize == identity` is a hard invariant, tested per format.
- We do **not** format, lint, or normalize. taplo and prettier own that.
- **An assigned string keeps the quote style of the string it replaces**, and
  falls back to another style only when the new value can't be spelled in the
  old one (#81). The fallback is always a style that spells any string
  (double-quoted, or TOML basic), so it never fails. Only strings carry a
  style: a number, bool, or null is written bare, since quoting it would change
  its type. A new key has no old style and takes the format's default. Per
  format:
  - **YAML**: double, single, and plain each keep themselves. Single quotes
    can't hold a control character or line break. Plain can't hold anything
    that would read as another type or meaning (`1.10`, `null`, `a: b`,
    `x #y`, a leading indicator such as `@` or `*`), a `,[]{}` inside a flow
    collection, or a word a YAML 1.1 reader types differently (`yes`/`off`,
    `1_000`, `12:30`, `2001-12-14`). That last rule is waived when the replaced
    scalar was the same kind of word, so a file relying on 1.1 booleans can
    flip `yes` to `no`. New keys and `-T yaml` quote those words too. The
    scalar's anchor always survives, and so does its tag, unless it is a core
    tag (`!!str`, `!!int`, ...) that no longer fits the value.
    A **block scalar** (`|` literal, `>` folded) stays one (#89): the new
    text goes on lines at the scalar's own content indent, under its own
    header (indicators and comment kept), and the blank lines after it stay.
    Nothing is reflowed. A line of `>` text is written as one line however
    long, since edikt can't know the width a file wraps at, and a line break
    in the value is written as the blank line `>` spells it with. The
    chomping indicator stays while it fits the value's trailing line breaks
    (none for `-`, exactly one for clip, any number for `+`) and changes to
    the one that fits when it doesn't, so the value reads back as assigned:
    `.notes = "x"` over `notes: |` writes `notes: |-`, and `.notes = "x\n"`
    keeps `|`. A `+` scalar owns the blank lines after it, so a value turned
    `+` absorbs them. Text that starts with a space or tab gets an indentation
    indicator. The new block must read back as the value; when it can't (a
    control character or `\r`, which a block scalar has no escape for, or an
    indentation indicator edikt can't place), the string falls back to
    double quotes on the header's line. A number, bool, null, or collection
    replaces the block the way it would a plain scalar on that line.
  - **TOML**: basic `"`, literal `'`, and the multi-line `"""`/`'''` forms
    keep themselves. A literal has no escapes, so a value it can't hold
    verbatim becomes basic, staying multi-line if it was.
  - **JSON5**: a single-quoted string stays single-quoted (any value fits).
  - **KDL**: quoted, raw (`#"..."#`, adding a `#` as needed), and bare
    identifier strings keep themselves. A bare string stays bare only while
    the value is a valid identifier. A multi-line `"""` string has no one-line
    form, so it becomes quoted.
  - **INI, `.env`, `envspaced`**: not applicable. A value is its bytes, and
    any quotes in them are part of the value (`a = "x"` reads as `"x"`), so an
    assignment writes exactly the string it is given. With no quoting to fall
    back on, a string these formats can't read back as itself **errors**
    rather than being written (see Per-format semantics).
- **Line endings are the file's.** An untouched line keeps its own ending,
  CRLF or LF, in a file that mixes them too (a multi-line string's lines
  included). A line an edit adds (a key, element, node, section, or comment)
  takes the file's **dominant** ending: CRLF when CRLF lines outnumber LF
  ones, else LF. This holds in every format and through the frontmatter lens.
  `toml_edit` writes every line LF, so the TOML crate restores each line's
  ending on output by matching lines against the source; the other formats
  splice, and spell only the new text with the dominant ending. The shared
  helpers are `edikt_core::text`.
- **A missing final newline stays missing** where an edit rewrites or extends
  the end of the file: TOML, YAML and KDL value edits, and a foot comment
  below the last line in any format. A foot comment added below an
  unterminated last line first ends that line, so it never lands on the
  value's own line (`A=1# x` would read back as the value `1# x`); a KDL
  node added after an unterminated node likewise ends it first. Appending
  a whole `.env` or INI entry writes it as a terminated line.
- **A UTF-8 byte-order mark is not content.** Each format's `parse` sets a
  leading BOM aside (so it is never part of the first key, and YAML and JSONC
  can read the file at all) and `to_source` puts it back, on every round-trip
  and edit. The frontmatter lens keeps it at the head of its opaque prefix.

---

## CLI contract

```
edikt [EXPR] [FILE...]
edikt -e EXPR [-e EXPR...] [FILE...]
edikt -f script.edk [-f ...] [FILE...]
```

| flag | meaning |
|---|---|
| *(positional EXPR)* | the expression, jq-style, when no `-e`/`-f` given; with `-e`/`-f` present, every operand is a FILE |
| `-e, --expr EXPR` | inline expression; repeatable; applied in order |
| `-f, --file PATH` | read a script (statements, newline/`;` separated); repeatable; composes with `-e` in order. Scripts may open with **header directives** - `toFormat: FMT`, `type: FMT` - which CLI flags override; `#` header lines (comments, shebangs) are skipped |
| `-i, --in-place[=SUFFIX]` | write result back to each FILE; `-i.bak` keeps a backup (sed/perl style). Requires FILE; errors on stdin. Needs a mutating expression or a conversion (`-T`) |
| `-t, --type FMT` | force **input** format (`jsonc`\|`json5`\|`json`\|`ini`\|`env`\|`properties`\|`envspaced`\|`toml`\|`yaml`\|`kdl`\|`markdown`; aliases `cfg`, `conf`, `props`, `spaced`, `yml`, `md`, `mdx`, `qmd`, `rmd`, `frontmatter`, `fm`; any case) |
| `-T, --to FMT` | **output** format (default: the input format, preserved). `--json`/`--jsonc`/`--ini`/`--toml`/`--yaml`/`--kdl` are shorthands for `-T <fmt>`. `markdown` is an input lens and never an output format |
| `-o, --output FILE` | write to FILE instead of stdout; queries/conversions infer the output format from FILE's extension (`-T` wins), mutations treat it as a sink. Nothing is written on a query miss |
| `-r, --raw` | force raw scalar output (default for scalars already) |
| `--strict` | when output format differs from input, lossy degradations (dropped comments, flattening) are errors (exit 2) instead of warnings |
| `--exit-status` | a query with no results exits 1 (jq-style presence test) instead of the default silent exit 0; likewise an edit through a path expression (`(.xs[] \| select(...) \| .n) = v`) that matched nothing exits 1, so a script can assert the edit landed |
| `--no-vivify` | assignments **fail** (exit 2) when the target path doesn't already exist, instead of auto-creating. Plain `=` is jq-style and creates missing keys by default (announced by a stderr `created` **note**); this opts out so a mistyped or wrongly-scoped path can't silently write a new key. An `arr[len] = v` TOML append is exempt (it's not a create); a document-level `select(`/`^dN` scope keeps default behavior, while a `select(...)` *inside* an assignment path is checked like any path (each path it resolves to that doesn't exist is a create); `|=`/`+=` already error on a missing target and `del` stays a no-op |

**Output-format precedence:** explicit CLI (`-T` / a `--fmt` shorthand) ->
`-o` FILE's extension -> script `toFormat:` directive -> the input format,
preserved. Input-format precedence:
`-t` -> script `type:` -> filename detection. `json` and `jsonc` are distinct
formats sharing one engine: JSON has no `Comments` capability, so JSONC -> JSON
is a real conversion that warns and drops comments.

**I/O defaulting (sed-shaped):** no FILE or `-` -> read stdin, write stdout.
FILE without `-i` -> read file, write result to stdout. `-i` -> write back per
file.

**Format detection:** `-t` wins; otherwise the file name (`.env` and `.env.*`
dotfiles), then the extension: `.json`, `.jsonc`/`.json5`, `.ini`/`.cfg`/`.conf`,
`.env`/`.properties`/`.props`, `.toml`, `.yaml`/`.yml`, `.kdl`,
`.md`/`.markdown`/`.mdx`/`.qmd`/`.rmd`, both in any case (`up.YAML`, `.ENV`). One alias
table (`FORMAT_ALIASES` in `crates/edikt/src/main.rs`) feeds `-t`/`-T`,
extension detection, and the error listing, so every detected extension is
also a `-t` name; `envspaced`/`spaced` and `frontmatter`/`fm` are `-t` names
only. There is no content sniffing: **stdin without `-t`,
or an unknown extension, is an error** (predictable beats magic). `envspaced` is
never auto-detected (see Per-format semantics).

---

## Expression language (jq-flavored, fuller v1)

Syntax family is jq/yq so nobody has to learn anything. Scope is capped: this is
an *edit* language, not a general-purpose one.

**Navigation**
- identity `.`
- field `.foo`, `.foo.bar`, `."quoted key"`, `.["key"]`
- **hyphenated bare keys where they cannot be arithmetic**: an assignment target
  (`.dev-dependencies.serde = "1"`, because an LHS must be a path) and an object
  key (`{default-features: false}`, because a key is a literal name and is never
  evaluated). In a **query** a hyphen still is subtraction (`.total-length`
  subtracts the `length` builtin), so a query quotes the key and gets a
  diagnostic saying so. Joining requires the tokens to abut: `.a - b` is always
  an operator.
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
  [`comments-as-first-class.md`](./comments-as-first-class.md)).
- comment stream `comments` - a document-wide stream of `{path, kind, text}`
  records over every comment (query: `comments | select(.text | test("TODO")) |
  .path` = which keys carry a TODO); as a mutation target, `comments |= gsub(...)`
  bulk-edits every comment's text and `del(comments)` clears them all.
- filter `.items[] | select(.enabled == true)`
- **document selector** `^dN`: a program prefix (`^d1.spec.replicas = 3`) that
  scopes the expression to one document of a multi-document YAML stream,
  0-based and strict (an out-of-range `^dN` is exit 2, not a no-op). Without
  it an expression maps over every document: a query yields one result per
  document, `=` applies to each (auto-vivifying, with the note naming the
  document), and `|=`/`+=` skip documents that lack the path. `select(...)` as the
  first stage targets documents by content
  (`select(.kind == "Service") | .spec.type = "LoadBalancer"`).

**Mutation**
- assign `PATH = <expr>` - the RHS is evaluated in the value calculus
- update-assign `PATH |= <expr>` - RHS sees the current value as `.`
- append `.arr += [<expr>]`
- delete `del(PATH)`

  **Mutations fan out over `[]`** exactly like jq: `.a[] |= f` maps `f` over
  every element, `.a[] += x` is `.a[] |= . + x`, `.a[] = x` sets every element,
  and `del(.a[])` empties the collection (each stays a single clean splice per
  node; creating *new* elements through `[]` still errors). YAML empties a
  block container to its inline form (`a: []`/`o: {}`) rather than a dangling
  `a:`-null. A fan-out delete resolves to concrete paths via
  `expand_delete_paths` (the iterate expansion, reversed back-to-front so
  indices stay valid as the collection shrinks). An auto-vivifying `=` is
  announced: the CLI prints `edikt: note: <file>: created `<path>` (was
  missing)` on stderr, so a wrong path is visible even when it "succeeds" -
  a scripted edit must assert that it applied. `--no-vivify` hard-fails
  instead (see the CLI contract).

  **Assignment through a path expression** (#88). The left side of `=`,
  `|=`, `+=` and the argument of `del(...)` may be a *path expression*,
  not only a plain path: paths, `[]`, `|`, `,`, and `select(pred)`,
  composed freely and parenthesized as in jq:
  `(.items[] | select(.id == "b") | .n) = 5`,
  `(.bin[] | select(.name | startswith("x")) | .path) |= ltrimstr("./")`,
  `del(.items[] | select(.stale))`. (A step can't follow a parenthesized group
  yet, so it is `(... | .n)`, not jq's `(...).n`.) The semantics are jq's: the left side
  first resolves, against the document as it stands before this assignment, to
  a **set of concrete paths** (`.items[1].n`, ...), and each is then edited on
  its own through the ordinary single-path splice, so every untouched byte
  survives exactly as for `.items[1].n = 5` typed by hand. `=` and `+=`
  evaluate the right side once, against the whole document; `|=` evaluates it
  per match against that match's value. `del` removes the matches back to
  front, so earlier indices stay valid.
  - **Resolution mirrors a query**: a key or index that isn't there resolves to
    its path anyway (so `(... | select(...) | .new) = 1` creates `.new` on
    each match, with the usual `created` note and `--no-vivify` check), but
    `[]` over something missing resolves to nothing, and `select` keeps a
    path when its predicate is truthy. Iterating or indexing the wrong type is
    an error, as in a query. Anything else on the left (a literal, arithmetic,
    another builtin) is still "left side of an assignment must be a path".
  - **Multiple matches** are each edited, in document order.
  - **Zero matches is a no-op**, like a query miss (exit 0), and it is
    announced on stderr (`edikt: note: <file>: .items[] | select(...) | .n
    matched nothing; no change`), since an edit that silently did nothing is
    as invisible as one that silently created a key. `--exit-status` turns it
    into exit 1, so a script can assert the edit landed. Over a
    multi-document stream the note fires only when no document matched. Like
    the `created` note, it isn't computed under a document-level
    `select(...)`/`^dN` scope, and it judges each assignment against the
    document as the program started, so a path created by an earlier
    statement of the same program isn't seen.
  - Comment steps (`.#`) don't compose with a path expression yet; a comment
    edit takes a plain path.

  `path(f)` is the same resolution as a query: it outputs each concrete path
  as a jq path array (`path(.items[] | select(.id == "b"))` is `["items",1]`),
  so a caller can see what an edit will touch. It reads paths; it is not a
  first-class path value you can assign through (no `getpath`/`setpath`).

**Value calculus** (what makes "fuller" fuller: the evaluator computes, it
doesn't just place literals):
- JSON literals: `"s"`, `1`, `1.5`, `true`, `false`, `null`, `[...]`, `{...}`,
  plus JSON5's non-finite number literals `Infinity` / `-Infinity` / `NaN`
  (numbers, never identifiers - a field literally named `Infinity` needs
  `."Infinity"` quoting, same rule as the JSON5 reader).
  An object entry spells its separator `key: value` (jq) or `key = value`
  (the TOML/KDL inline-table hand), and a **bare key with no separator is
  jq's pluck shorthand**: `{MemoryMiB, UseGrpcfuse}` is
  `{MemoryMiB: .MemoryMiB, UseGrpcfuse: .UseGrpcfuse}`, the common way to
  select a few keys. Quoted (`{"a.b"}`) and hyphenated (`{default-features}`)
  keys pluck too; a key the input lacks yields `null`, as in jq
- arithmetic on numbers: `+ - * / %`
- string concat with `+`
- a small function registry, jq-named: `length`, `keys`, `has`, `type`,
  `tostring`, `tonumber`, `ascii_upcase`, `ascii_downcase`, `ltrimstr`,
  `rtrimstr`, `startswith`, `endswith`, `split`, `join`, and the regex family
  `test`, `match`, `capture`, `sub`, `gsub`, plus `path(f)` (see Mutation) (args `;`-separated, jq-style;
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
values (`path(f)` reads paths; nothing assigns through a path value), module
imports. If the language starts wanting these, that's
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

- **JSONC / JSON5 / JSON** - full typed model, one lexer/parser for the family.
  `.json` is read by the JSONC parser (superset); it just has no comments to
  preserve. **JSON5** adds unquoted object keys (ASCII `IdentifierName`,
  reserved words included), single-quoted strings, backslash-newline line
  continuations, and hex / leading-dot / trailing-dot / `+`-signed /
  `Infinity` / `NaN` numbers. Since the family shares a grammar, leniency is
  input-only and uniform: a `.jsonc` file using a JSON5 spelling parses rather
  than erroring, and nothing is ever *emitted* in a spelling the file did not
  already use. Unicode and `\u`-escaped identifiers are deliberately not lexed
  (an error token) rather than half-supported. A bare word is a key spelling
  only, never a value, so `foo` alone is still not a document. That leniency
  (trailing commas, comments, JSON5 spellings) is the whole of it: structure is
  checked, and a missing `:` or `,`, a stray token where a key or value
  belongs, an unclosed container, or anything but whitespace and comments after
  the top-level value is a parse error (exit 2) naming its line and column,
  never a document an edit would splice into.
  Highest-value target (`tsconfig.json`, `settings.json`, `devcontainer.json`).
  **Non-finite numbers** (`Infinity`/`NaN`) exist only in JSON5: raw output and
  the JSONC/JSON5-family emitters keep the JSON5 spelling, and only strict `-T
  json` encodes them as `null` (`JSON.stringify` parity) - that degradation
  warns (it is fatal under `--strict`), since JSON's grammar has no such
  literal. **Inserting** a non-finite value follows the same dialect rule at
  the document level: the source is flagged `json5` at parse if it uses any
  JSON5-only spelling (unquoted keys, single quotes, `+`/hex/leading- or
  trailing-dot numbers, `Infinity`/`NaN`, line continuations) - comments and
  trailing commas alone are JSONC, not JSON5 - and an insert into a `json5`
  document keeps the literal while one into a strict document *errors* rather
  than silently writing `null` (the moat's never-silently-drop rule, at
  mutation time).
- **INI** - paths are `.section.key`; sectionless preamble keys are top-level.
  Values are strings. No arrays/objects; an array index into INI is a clean
  error (exit 2). Iteration over a section's keys is allowed. INI has no
  quoting, so an assignment it can't read back as written **errors** (exit
  2) instead of silently writing something else: a value with a line break,
  leading or trailing whitespace (read back trimmed), or a `;`/`#` at its
  start or after whitespace (read back as an inline comment); a new key that
  starts with `[`, `;` or `#` (a header or comment), or holds `=`, `:`, a
  line break, or surrounding whitespace; a new section name holding `]` or a
  line break. `-T ini` output follows the same rules: a key, value or section
  name it would have to write that way is an error naming the formats that
  can hold it.
- **`envspaced`** - the `.env` document model with a **whitespace separator**
  (`Port 22`), for `sshd_config`-shaped daemon configs. Shares `edikt-env`
  entirely; a `Dialect` picks only how the key ends, since the separator is the
  whole difference. The first run of spaces/tabs ends the key and the rest of
  the line is the value, so `Subsystem sftp /usr/lib/sftp-server` is one value.
  A document remembers its dialect, so an appended key is spelled the way the
  file spells its existing ones. **Never auto-detected**: `sshd_config` has no
  extension, `.conf` is already an INI alias, and a `key value` line is
  indistinguishable from a malformed `.env` line, so guessing would silently
  edit the wrong bytes - it is `-t envspaced` or nothing. Deliberately **not**
  an `ssh_config` parser: `Match`/`Host` blocks scope the keys beneath them and
  this model is flat, so such a file is out of scope rather than
  half-supported, on the same reasoning that keeps interpolation out of `.env`.
- **`.env` / `.properties`** - flat `.KEY`, string values, **line-level editing
  only, forever.** No grammar, no interpolation, no quoting semantics, no type
  coercion in storage. Set the bytes after the separator; preserve everything
  else. (There is no single `.env` grammar: docker-compose, dotenv libs, and
  shell `source` disagree, so "correctly" parsing it is a bottomless bug queue.
  We don't.) The flip side of no quoting: a value or key the line scanner
  would read back differently **errors** (exit 2), never gets invented
  quotes. That is a value with a line break (it would inject another entry)
  or leading/trailing whitespace (read back trimmed), and a new key that is
  empty (`envspaced`), starts with `#`/`!`, holds the separator (`=`/`:`, or
  whitespace in `envspaced`), a line break, or surrounding whitespace. `-T env`
  and `-T envspaced` output refuse the same keys and values.
- **YAML** - a byte splice over the span tree (see Architecture). Assigning
  a **mapping or sequence** writes it in the file's own layout (#83): **flow
  under flow** (a slot inside `[...]`/`{...}`, or a value that already was a
  flow collection, takes the single-line flow spelling, and a key or item added
  to a single-line flow collection joins it in place), **block under block**
  otherwise, indented like the file: the most common nesting offset among its
  existing blocks, and sequences at their key's own column (`key:\n- a`) if
  that is how the file writes them; two spaces when nothing is nested to learn
  from. An empty collection is `[]`/`{}`, and a sequence item holding a
  collection opens on its dash line (`- k: v`, `- - x`). A comment beside a
  replaced scalar stays on its line, a key's comment stays on the key line when
  its block becomes a scalar, and comments inside a replaced block go with it,
  as with `del(.a[])`. A replacement that keeps a collection's shape (the same
  kind, its existing keys in order or its existing items, anything new after
  them) edits only the elements that change, so `.tags |= . + ["x"]` appends
  one line and assigning a collection its own value changes no bytes; any
  other replacement rewrites the collection, keeping its anchor. A new block
  key or item goes directly after the collection's last content line, so the
  blank lines and comments that separate it from what follows stay after the
  insertion (#90). The blank lines after a block scalar count as separators,
  not content, unless it keeps them (`|+`/`>+`), since there they are part of
  its value. Plain `=`
  creates missing parent mappings at any depth (#85), laid out by the same
  rules, in every document of a stream; it can't create an array element or
  a key inside a scalar. A block (`|`/`>`) scalar is set in place, keeping
  its style (see the moat). Refused rather than reflowed: replacing a quoted
  or plain scalar that wraps over several lines, and growing a multi-line
  flow collection.
- **TOML** - lossless via `toml_edit`'s decor-preserving DOM. An array grows
  in its own layout (#91): `.a += [x]`, `.a[len] = x`, and any assignment
  whose new array keeps the old elements as a prefix append rather than
  rewrite, so the kept elements stay byte for byte (assigning an array its own
  value changes nothing). In a one-item-per-line array (its last item opens
  its own line) the new item gets its own line at that item's indentation; an
  empty array whose `]` is on its own line indents one level (four spaces)
  past the bracket. An inline array stays inline, separated like its last
  item. The trailing-comma style is kept (a fresh multi-line list takes one),
  and whatever sat between the last item and `]` stays ahead of the bracket,
  so a comment beside the old last item stays beside it. An array of tables
  grows by a `[[key]]` block per new table. Any other array assignment
  rewrites the array inline, keeping the value's decor. A table that `=`
  auto-vivifies follows its surroundings: inside a dotted table, or beside a
  sibling table spelled as dotted keys (`edition.workspace = true`), it is a
  dotted key too (`rust-version.workspace = true`); with no dotted precedent
  it gets its own `[a.b]` header (intermediates stay implicit, so `.a.b.c = 1`
  writes only `[a.b]`).
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
  reflowed. A new node copies the layout of the sibling it follows (its
  indent on a line of its own, or a `;`-separated slot in a single-line
  `{ a 1; b 2 }` block), and anything nested inside it indents by the file's
  own unit. Deleting a block's first child keeps the `{` line intact.
- **Frontmatter** (`edikt-frontmatter`, `-t markdown`): a **lens**, not a
  format. It splits the file into an opaque opening fence, the metadata block,
  and an opaque suffix (closing fence plus the whole body), hands the block to
  the YAML/TOML/JSONC engine, and re-splices on serialize, so the body's bytes
  are never touched. Containers: `---` YAML (closed by `---` or `...`), `+++`
  TOML, tagged `---yaml`/`---toml`/`---json`, a bare `{ ... }` JSON object at
  byte 0, and PEP 723 `# /// name` ... `# ///` blocks in a host-language file
  (TOML once the `# ` prefix is stripped; re-applied on write). `.md`,
  `.markdown`, `.mdx`, `.qmd`, `.rmd` auto-detect; anything else (a `.py`
  script) needs `-t markdown`. It is input-only: `-T markdown` errors and names
  the block's own language as the way out. Capabilities are the inner block's.

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
  delete, append); the **mutation driver** (`apply_mutation` interprets
  `=` / `|=` / `+=` / `del` / `|` and path-expression targets once, over the
  `Mutable` primitives each format supplies: value-at, set, delete, add). There
  is no conversion trait: each format crate exports `emit` / `emit_commented`
  free functions, and shared data-model helpers live in `edikt-core`'s
  `convert` module.
- **`edikt-syntax`** (lib) - shared **rowan** substrate: green-tree helpers,
  generic lossless serialize (walk green tree -> concat token text), splice /
  structural-sharing edit utilities usable by any format's `SyntaxKind`.
**Library surface (the crates are a public API, not just the binary's guts).**
Every format crate re-exports the `edikt-core` types that appear in its own
signatures - `Value`, `Step`, `Expr`, `Document`, `Feature`, `CommentKind`,
`Commented`, `EditError`, `Mutable` (where the document implements it), the
`json!` macro, and `parse as parse_expr` (aliased because each crate's own
`parse` is its document parser). A dependent calls
`Jsonc::set` without also taking a direct `edikt-core` dependency. `json!` is
the `serde_json`-shaped `Value` constructor; it builds a data-model value, never
a document, since the CST is what round-trips bytes.

- **`edikt-jsonc` / `edikt-ini` / `edikt-env`** - each builds a rowan tree over
  `edikt-syntax`: `edikt-jsonc` from a `logos` lexer, `edikt-ini` and
  `edikt-env` from hand-written line scanners (their grammars are
  line-oriented and context-sensitive). Each has typed AST accessors, a static
  `FEATURES: &[Feature]`, a `Document` impl, and its emitters.
- **`edikt-toml`** - `Document` + emitters over `toml_edit`'s decor-preserving
  DOM (edits keep comments/layout; no rowan needed; `toml_edit` is the CST).
- **`edikt-kdl`** - `Document` + emitters over `kdl-rs`'s format-preserving
  document (same pattern as TOML: the library is the CST; per-node `leading` /
  `before_terminator` decor carries the comment model).
- **`edikt-yaml`** - pure Rust over `libyaml-safer`. Not a rowan CST: one parse
  pass composes the event stream into a **span tree** (every scalar/collection's
  byte range) that doubles as the data model *and* the edit map. Edits are a byte
  splice over the original source (untouched bytes preserved verbatim); merge
  keys (`<<`) resolve in the value projection. Same `Document` seam, so
  the CLI dispatches over it identically to the rowan formats. A
  multi-document stream parses into one `Yaml` holding a span tree per
  document, which is what `^dN` and document-level `select(...)` scope.
- **`edikt-frontmatter`** - the frontmatter lens: a `Document` that delegates to
  the YAML/TOML/JSONC crate for the block and re-splices the opaque prefix and
  suffix in `to_source`.

**Why rowan:** lossless-by-construction CST, edit = structural-sharing splice
(untouched nodes are the *same* green nodes, so byte-identity holds by
construction, not bookkeeping), one framework across the hand-built formats.
This is the taplo/rust-analyzer pattern.

---

## Out of scope (say no to these)

- Being jq - no general-purpose/functional language (see v1 scope list above).
- Formatting / linting / reflowing - ever, for regions not targeted.
