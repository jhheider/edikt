# edikt

*edit, meets edict.*

A format-preserving structured-config editor for the formats the current tooling
edits badly or not at all — **JSONC/JSON5**, **INI**, and sectionless key-value
files (**`.env`**, **`.properties`**). It edits with a jq-flavored expression
language and a sed-flavored execution model, and it **never reflows what it
didn't touch**.

```sh
# query — reads like jq
edikt '.compilerOptions.strict' tsconfig.json

# edit in place — comments, indent, comma style all preserved
edikt -i '.compilerOptions.target = "ES2022"' tsconfig.json

# compute, not just place
edikt -i '.version |= . + "-dev"' package.jsonc

# stream-first, like sed
cat settings.jsonc | edikt 'del(.telemetry) | .theme = "dark"'

# script from a file
edikt -f release.edk -i config.jsonc

# convert, where feasible (data-model; trivia dropped)
edikt -t ini -T json app.cfg
```

**The gap it fills:** jq has no concept of a comment; yq hard-errors on `//`;
nothing edits JSONC (`settings.json`, `tsconfig.json`, `devcontainer.json`)
without clobbering a human's formatting. That's the product.

Status: **pre-alpha, in design.** See [`CLAUDE.md`](./CLAUDE.md) for the build
contract.

## License

TBD.
