# Validation Guide

Read when changing Python tests, running S3 end-to-end tests or full CI, or investigating test failures. Basic Rust checks and checks for documentation-only changes are in the [root guide](../../AGENTS.md). Commands run from the repository root.

## Python and end-to-end tests

- Check syntax with `python3 -m py_compile <changed_file.py>` when modifying Python scripts.
- Run affected end-to-end tests when an S3-compatible test environment (MinIO / RustFS) is available. See the [test documentation](../../tests/README.md) for setup, credentials, test selection, and cleanup.
- `python3 tests/cli_e2e.py --offline` runs checks that do not need S3. Do not report an offline pass as real S3 validation.
- For CLI parameter changes, also follow the [CLI validation requirements](cli-conventions.md#documentation-and-validation).

## Full CI

For cross-module changes requiring full CI validation, run:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

The [CI workflow](../../.github/workflows/ci.yml) defines the complete platform, diagnostics, S3 end-to-end, and Docker checks.

## Failures and reporting

Investigate or recheck concurrent test failures and report the original failure and rerun conditions. A serial rerun passing does not mean the concurrent run passed. Report actual checks completed and identify checks not run.
