# edikt

**Edit config files without reflowing them.**

*edit, meets edict.* A lossless, format-preserving config editor for
**JSONC/JSON5**, **INI**, **TOML**, **YAML**, and sectionless key-value files
(**`.env`**, **`.properties`**). It edits with a jq-flavored expression language
and a sed-flavored execution model, changing only the bytes you target —
comments, indentation, quoting, and trailing commas in every untouched region
survive byte-for-byte.

```sh
# query — reads like jq
edikt '.compilerOptions.strict' tsconfig.json

# edit in place — comments, indent, comma style all preserved
edikt -i '.compilerOptions.target = "ES2022"' tsconfig.json

# edit YAML in place — anchors, flow style, and comments survive
edikt -i '.services.web.replicas = 3' compose.yaml

# compute, not just place
edikt -i '.version |= . + "-dev"' package.jsonc

# stream-first, like sed
cat settings.jsonc | edikt 'del(.telemetry) | .theme = "dark"'

# script from a file
edikt -f release.edk -i config.jsonc

# convert, where feasible — comments carried across, in the target's syntax
edikt -T yaml tsconfig.jsonc
```

**What it does:** edits commented, hand-formatted config — `settings.json`,
`tsconfig.json`, `devcontainer.json`, `compose.yaml`, `Cargo.toml` — and writes
back a file that is byte-identical except for the one value you changed.
Comments, trailing commas, indentation, and quoting all survive. Query it like
jq; convert between formats with `-T`.

Status: **alpha** — six formats with lossless in-place edit, query, and
conversion (comments carried across); release infra is wired and awaiting the
first cut. See [`CLAUDE.md`](./CLAUDE.md) for the build contract.

## Scripting notes

- **Exit codes are grep-shaped:** `0` = success / at least one match, `1` = a
  query that matched nothing, `2` = parse or evaluation error. Under `set -e`,
  guard optional lookups: `edikt '.maybe.key' f.yaml || true`.
- **The expression language is deliberately capped in v1.** jq's navigation,
  mutation, arithmetic, and a curated builtin registry (including regex
  `test`/`match`/`capture`/`sub`/`gsub`, `split`/`join`) are in; jq's
  `//` alternative operator, variables (`as $x`), `if/then`, `reduce`, and
  user-defined functions are not (yet). For "value or default", test first:
  `edikt '.key' f.yaml || echo default`.
- **`.env` is flat and string-valued** — no arrays or nesting, ever — but
  string computation on values works fine:
  `edikt -i '.VERSION |= sub("^v"; "")' .env`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
