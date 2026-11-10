# Repository Guide

ponydb runs SQLite on versioned S3 storage through a custom VFS, with CLI, HTTP, and MCP entry points. Use Rust 1.95 and run commands from the repository root. See [README.md](README.md) for setup and module responsibilities.

## Essential constraints

- Database correctness depends on conditional writes to S3 HEAD. Local caches and working directories must remain disposable.
- CLI, HTTP, and MCP share `database-service`, `protocol`, and `runtime-config`. Check affected entry points when changing shared behavior.

## Read by task

Read only the references relevant to the task. Check directory-scoped `AGENTS.md` files before editing their subtree. More specific guidance takes precedence on the same subject; explicit user instructions take precedence over repository guidance.

| When working on | Reference |
| --- | --- |
| Documentation | [Documentation standards](docs/agents/documentation-standards.md) |
| Release versions, tags, publication, or release failures | [Release guidance](docs/agents/releases.md) |
| Code | [Coding standards](docs/agents/coding-standards.md) |
| CLI commands, help, usage examples, or related tests anywhere in the repository | [CLI conventions](docs/agents/cli-conventions.md) |
| SQLite file restore, including progress and tests | [Restore constraints](docs/agents/file-restore.md) |
| Python tests, S3 end-to-end tests, full CI, or test failures | [Validation guide](docs/agents/validation.md) |
| HTTP / MCP services | [Service documentation](apps/cli/SERVER.md) |
| Online GC | [GC design](docs/online-gc.md) |

## Checks and maintenance

- For every change, run `git diff --check` and verify links in changed documents. Documentation-only changes do not require builds or code tests.
- For Rust changes, run `cargo fmt --all` and `cargo test --locked -p <affected-package>`. Use `ponydb` for CLI changes; additional checks are linked above.
- Report checks run, failures, and checks not run.
- Keep this file and directory-scoped entry points short. Store detailed AI Agent guidance for developing PonyDB source code in `docs/agents/` with explicit reading triggers, and prefer references to existing documentation or code over copied examples and directory listings.
