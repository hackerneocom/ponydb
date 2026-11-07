# SQLite File Restore Constraints

Read this reference only when changing file restore implementation, progress, usage examples, or tests. Changes involving `apps/cli` must also follow the [CLI conventions](cli-conventions.md).

## Entry point and publication

- Only the CLI exposes `db DATABASE_NAME restore file SQLITE_FILE`. HTTP and MCP do not expose restore. Preserve existing compaction, GC, and snapshot export interfaces.
- Input must be a stable, complete, standalone SQLite backup using DELETE journal mode. Require the positional file; a DTO cannot replace it, and `-` does not mean stdin for the database file.
- The target must be absent or deleted. Never overwrite an active database.
- Follow this sequence: copy to a local temporary file → check SQLite integrity, foreign keys, and schema compatibility → compute SHA-256 → upload formal data segments directly to S3 → build the index, manifest, and first snapshot → conditionally publish HEAD.
- Do not introduce staging uploads, file downloads, restore sessions, or resume files. Upload reaching 100% is not completion; wait for successful HEAD publication.

## Timeouts, cancellation, and failure

- Restore has no total execution deadline. Reject `--deadline-ms` and do not apply request timeout defaults. Individual S3 operations still have timeouts.
- Do not configure restore file-size or disk-usage quotas. Fail on insufficient temporary disk space and clean up temporary files.
- Ctrl+C stops local work and further uploads. Publication already in flight must still have its outcome confirmed.
- Later GC can reclaim unpublished objects once a database HEAD exists.
- After a definitive failure, rerun the complete command. For `OUTCOME_UNKNOWN`, inspect the database before deciding whether to retry.

## Output and progress

- Always write stages and progress to stderr. Stdout contains only the final JSON / table result, with fields `committed`, `outcome`, `name`, `data_version`, `total_bytes`, `sha256`, and `page_size`.
- Refresh interactive terminals in place at most once every 200 milliseconds. With redirected stderr, report stage changes immediately and progress within a stage at most once every five seconds.
- Show bytes and percentages for copying, hashing, and confirmed data-segment uploads. Show the stage and elapsed time for validation, index building, and publication.

## Implementation references

Consult as needed: CLI orchestration in `../../apps/cli/src/main.rs`, progress in `../../apps/cli/src/restore_progress.rs`, business logic in `../../crates/database-service/src/restore.rs`, and end-to-end tests in `../../tests/cli_e2e/test_restore.py`. See CLI conventions for validation and documentation requirements.
