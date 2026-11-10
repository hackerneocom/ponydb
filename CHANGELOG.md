# Changelog

## 0.1.1

- Publish under a new version because GitHub permanently reserves `v0.1.0` after
  its use by an immutable release.
- Retain the CLI, HTTP, MCP, and storage behavior from 0.1.0. Storage layout
  version remains 1; no configuration changes or data migration are required.

## 0.1.0

- Initial ponydb release: SQLite databases on S3-compatible versioned storage.
- Native CLI, HTTP API, and HTTP/stdio MCP entry points.
- Precompiled Linux, macOS, and Windows CLI packages and Linux multi-platform containers.

Storage layout version: 1. See the [configuration template](examples/ponydb.toml)
and [installation guide](skills/ponydb/references/installation.md) before deployment.
