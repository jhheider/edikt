set fallback

# edikt task runner. `just --list` to see recipes.

# Run the full workspace test suite.
test:
  cargo test --workspace --all-features

# fmt + clippy the way CI does (warnings are errors).
lint:
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings

# Generate coverage/lcov.info (matches the CI Coverage job; uses cargo-llvm-cov,
# install with `cargo install cargo-llvm-cov` or `taiki-e/install-action`).
coverage:
  mkdir -p coverage
  cargo llvm-cov --workspace --all-features --lcov --output-path coverage/lcov.info

# A human-readable coverage summary (per file), no file written.
coverage-summary:
  cargo llvm-cov --workspace --all-features --summary-only

# The uncovered lines per file, for finding gaps to fixture.
coverage-missing:
  cargo llvm-cov report --show-missing-lines
