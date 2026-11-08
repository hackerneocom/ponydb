# Release Guidance

Read when changing release versions, tags, publication tooling, or investigating release failures. Commands run from the repository root. See the [maintainer release guide](../releases.md#publish-a-version) for the publication procedure, [release script](../../scripts/release.py) for validation rules, and [workflow](../../.github/workflows/release.yml) for job ordering.

## Version and documentation updates

- Update `workspace.package.version` in [Cargo.toml](../../Cargo.toml). Refresh workspace package versions with `cargo update --offline --workspace`; inspect [Cargo.lock](../../Cargo.lock) to confirm external dependencies did not change.
- Add an exact version heading to [CHANGELOG.md](../../CHANGELOG.md), including storage compatibility and configuration or migration requirements. Preserve previous release entries.
- Search all repository documentation, including hidden directories, for the previous version. Update current download, container, and tag examples in both languages, including the root READMEs and release guides. Check module and service documentation for additional references; a release-guide update alone is insufficient.
- Retain old version numbers when they describe historical releases or fixed test fixtures. Do not replace unrelated dependency versions, protocol versions, or storage layout versions.
- Follow [documentation standards](documentation-standards.md) and verify links in changed documents.

## Failure diagnosis and recovery

- Inspect the failed job logs and earlier job results to locate the failing operation. Distinguish compilation, verification, registry promotion, and GitHub Release publication failures.
- For `tag_name was used by an immutable release`, use a new version. Deleting the original release or tag does not free the name, and rerunning the same version cannot resolve the error. See [GitHub's immutable release rules](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases).
- Check the current draft, checkpoint, and registry state before recovery. Publication is not atomic across GitHub and the registries: images may already be promoted when final Release publication fails. Follow the [recovery procedure](../releases.md#recover-a-failed-release) for recoverable failures; preserve existing published assets and version tags.
- Prepare and validate the new version before tagging. Create its tag on the merged `main` commit and push that specific tag to trigger the workflow. Do not tag an existing commit while the version bump is still uncommitted. Manual recovery must select the same tag as both workflow ref and `tag` input.

## Validation and reporting

For a version update, run:

```sh
cargo metadata --offline --locked --no-deps --format-version 1
python3 scripts/release.py prepare --dry-run
python3 -m unittest discover -s tests/release -v
git diff --check
```

Verify all workspace package versions match the intended release, review lockfile changes, and repeat the documentation search to explain remaining references to the previous version. `prepare --dry-run` checks version metadata, lockfile consistency, and release notes; it does not build packages, validate tag ancestry, or run the full workflow dry run.

Apply the [root checks](../../AGENTS.md#checks-and-maintenance) and [validation guide](validation.md) when changing code or tooling. Guidance-only edits do not require builds or code tests. Report checks run, failures, and checks not run, and distinguish local preparation from tag creation, pushing, and completed publication.
