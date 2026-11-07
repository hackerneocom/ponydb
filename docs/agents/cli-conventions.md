# CLI Conventions

Read when changing CLI commands, arguments, help text, usage examples, or related tests anywhere in the repository. Before changing file restore, also read the [restore constraints](file-restore.md).

## Positional arguments and options

- Use positional arguments for objects with a clear role, such as database names, SQL, files, and snapshot IDs. Use named options for configuration, context, filters, and limits.
- Use flags without values for boolean behavior, such as `--include-deleted`. Define clear defaults and preserve the distinction between omitted and explicitly supplied values when merging DTOs.
- Decide whether input is required according to business semantics, independently of whether it is an argument or an option. Avoid ambiguous combinations of optional positional arguments.
- Use kebab-case for long options and multiword commands. Use clear help placeholders such as `NAME` and `FILE`. Option values are not separate positional arguments.
- Require an explicit database name; do not introduce database context configuration. Use `db DATABASE_NAME [ACTION]` for single-database commands, with no ACTION returning details. Use `databases create DATABASE_NAME` and `databases list` for creation and listing.

See the [CLI argument conventions](../../apps/cli/README.md#argument-conventions) for usage examples.

## Definitions and validation

- Give each command a Clap `Args` type containing only its valid parameters. Share types only when semantics and constraints match. Conversion to a shared internal input structure after parsing is allowed; attaching every field to every command and merely hiding irrelevant help is not.
- Reject unrelated options and keep help consistent with accepted input. Validate global options against the final subcommand, including when they appear before it.
- Prefer parser-level declarations for required inputs, conflicts, conditional requirements, and enumerated values. Continue strict validation of file contents, DTO fields, and business types. Complete local input validation before accessing S3.
- Update and delete accept optional `--if-match`; validate the object ETag when supplied.
- Compaction budgets and segment limits, and GC budgets and object limits, may come from options or DTOs. After merging, apply existing defaults to missing fields and validate ranges; do not make these options required. See the [CLI README](../../apps/cli/README.md) for defaults. GC has no `--allow-pause`, recovery, or resume entry points.
- Allow `--deadline-ms` only for business commands other than restore, and for `mcp`. Reject it for `config`, `version`, `server`, and restore. Reject `--output` for `server` and `mcp`.

## Files and DTO input

- Use `--input FILE|-` for complete request DTOs. DTOs may supply required positional and option fields other than the database name. Require the explicit database name to match the DTO and validate completeness after reading.
- `db DATABASE_NAME restore file SQLITE_FILE` requires the positional file; a DTO cannot replace it.
- `db DATABASE_NAME execute` accepts either positional SQL or `--sql-file FILE|-`, mutually exclusively. SQL beginning with `-` may follow `--`.
- Use structured `--input` requests for multiple statements. Do not construct transactions by splitting SQL on semicolons.
- Allow repeated `--param` values, mutually exclusive with `--params-file`. Use `--metadata-file` for metadata JSON.
- Use `-` for file inputs that support stdin, with at most one stdin source per invocation. Do not extend this convention to restore database files.
- Parse JSON strictly: reject duplicate keys, unknown fields, and invalid types. Reject conflicting values supplied through both a DTO and explicit arguments instead of silently overriding them.

## Interaction and output

- Remove obsolete aliases and duplicate entry points instead of retaining them for compatibility. Update callers, documentation, and tests with the change.
- Avoid switches that offer no meaningful choice: `config show` always redacts secrets, and `mcp` uses stdio directly.
- Write successful results to stdout and errors to stderr. MCP stdout contains protocol messages only.
- Preserve existing exit codes. Use explicit flags for business operations requiring authorization; do not change script behavior through implicit interactive prompts.

## Documentation and validation

- When CLI behavior changes, update the English and Chinese READMEs in the root and `apps/cli/`, affected design documents, and affected examples and tests in `tests/cli_e2e.py` and `tests/cli_e2e/`.
- Cover positional mappings, rejection of unrelated options, required and conflicting inputs, DTO alternatives and conflicts, global option placement, and relevant defaults when changing parameter behavior.
- Check Clap definitions with `cli_command().debug_assert()` and inspect actual `--help` output for affected commands. Existing definition and argument tests are in `../../apps/cli/src/main.rs`; cross-command end-to-end checks are in `../../tests/cli_e2e/test_arguments.py`.
- After changing CLI code or parameter behavior, apply the root guide's Rust checks with package `ponydb`. See the [validation guide](validation.md) for test failures and additional checks.
- When changing Python tests, follow the [validation guide](validation.md).
