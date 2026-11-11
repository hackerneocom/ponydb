# Documentation Standards

Read when writing, editing, or reviewing documentation.

## Scope and document structure

- Keep paragraphs short and focused. Break up long passages with paragraph breaks and use line breaks where they improve readability.
- Except for implementation plans, documentation should describe the current state of the code by default. Record change history only in documents dedicated to that purpose.
- Organize Agent documentation by purpose and audience:
  - `docs/agents/`: guidance for Agents developing PonyDB source code. Use root or directory-scoped `AGENTS.md` files as entry points linking to the relevant references.
  - [`skills/ponydb/`](../../skills/ponydb/SKILL.md): the independently installable Agent Skill for PonyDB users, including its [setup runbook](../../skills/ponydb/references/setup.md) and usage references. Keep the complete skill directory together so it can be installed and used independently of the source checkout.
  - [`docs/install-ponydb-with-ai.md`](../install-ponydb-with-ai.md): the compatibility entry point directing external Agents to the setup runbook bundled with the skill.
- Preserve these locations when editing documentation. An Agent audience alone is not a reason to move user Skill content or the setup runbook into `docs/agents/`.
- Keep human-facing documentation focused on how to use the project. Do not include implementation details; put source-development guidance for AI Agents in `docs/agents/`.

## Language and terminology

- Write documentation in English by default.
- When a filename or the document itself explicitly specifies another language, use that language. For example, `*_zh-CN.md` documents must use Simplified Chinese.

## Keeping documentation current

- When editing a document with corresponding versions in other languages, update all existing language versions in the same change so their content, examples, and links stay consistent.
- When behavior or interfaces change, update the corresponding English and Chinese READMEs, affected design documents, callers, and tests.
- When changing release versions or versioned examples, follow the repository-wide documentation audit in [Release guidance](releases.md#version-and-documentation-updates).
- Changes confined to repository development guidance do not require rewriting product documentation.
- Keep repository Agent entry points short and task-specific details in linked references. Prefer links to existing documentation and code over copied examples or directory listings.
