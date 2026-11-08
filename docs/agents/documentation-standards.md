# Documentation Standards

Read when writing, editing, or reviewing documentation.

## Scope and document structure

- Except for implementation plans, documentation should describe the current state of the code by default. Record change history only in documents dedicated to that purpose.
- Store documentation intended for AI agents in `docs/agents/`. Use root or directory-scoped `AGENTS.md` files as entry points linking to the relevant references.
- Keep human-facing documentation focused on how to use the project. Do not include implementation details; put agent-facing implementation guidance in `docs/agents/`.

## Language and terminology

- Write documentation in English by default.
- When a filename or the document itself explicitly specifies another language, use that language. For example, `*_zh-CN.md` documents must use Simplified Chinese.

## Keeping documentation current

- When behavior or interfaces change, update the corresponding English and Chinese READMEs, affected design documents, callers, and tests.
- When changing release versions or versioned examples, follow the repository-wide documentation audit in [Release guidance](releases.md#version-and-documentation-updates).
- Changes confined to agent guidance do not require rewriting product documentation.
- Keep agent entry points short and task-specific details in linked references. Prefer links to existing documentation and code over copied examples or directory listings.
