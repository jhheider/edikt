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

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
