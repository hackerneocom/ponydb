# Maintainer release guide

[中文](releases.zh-CN.md)

This guide covers maintainer setup, version publication, and recovery from failed releases. To install the CLI or container images, see the [installation guide](../skills/ponydb/references/installation.md).

ponydb releases contain native CLI packages and Linux container images built from
the same tagged commit. Release automation runs in [GitHub Actions](../.github/workflows/release.yml).

## Maintainer setup

Before the first release:

1. Create a public Docker Hub repository named `ponydb`. Set the repository
   variable `DOCKERHUB_NAMESPACE` to its owner or organization.
2. Create a GitHub Environment named `release`. Set `DOCKERHUB_USERNAME` as an
   environment variable and `DOCKERHUB_TOKEN` as an environment secret. Use a
   token with write access to the target Docker Hub repository. No manual
   approval is required by the workflow; configure the environment accordingly.
3. Allow the `release` environment to deploy from `v*` tags. For recovery from a
   manually dispatched run, select that same tag in the Actions **Run workflow**
   ref selector, as well as entering it in the `tag` input.
4. Protect `main` and `v*` tags with repository rules. Limit tag creation to
   maintainers/release automation and block tag deletion and force updates.
   Enable immutable releases in repository settings.
5. Ensure Actions can write repository contents in the draft-discovery job and
   contents, packages, and attestations in the publication jobs. GHCR uses the
   workflow's `GITHUB_TOKEN`, not a PAT.
6. Make the GHCR `ponydb` package public. GHCR can initially create a private
   package even in a public repository. If the package does not exist yet, the
   first RC run creates the candidate and draft, then stops at the anonymous-pull
   checks. Set the package visibility to public and rerun the failed jobs.

The workflow and its reusable CI pin action revisions. The runtime Debian base
and smoke-test image indexes are pinned by digest; update those pins deliberately
to pick up OS fixes. Build the new image under a new ponydb version so existing
version tags stay unchanged.

## Publish a version

1. Update `workspace.package.version` in the root `Cargo.toml`, refresh the
   workspace package versions in `Cargo.lock`, and add a matching `## X.Y.Z`
   section to [CHANGELOG.md](../CHANGELOG.md). Prereleases need their own exact
   heading, such as `## 0.2.0-rc.1`.
2. Record configuration changes, storage layout compatibility, upgrade steps,
   and rollback limits in those notes. Update the English and Chinese usage
   documents when behavior changes. Run normal CI and merge to `main`.
3. Run **Release** manually with `dry_run` enabled before the first publication
   or after changing release tooling. This builds all five packages, tests both
   Linux architectures, and exports a multi-platform OCI archive without
   contacting a publishing endpoint or requiring registry secrets. Artifacts
   remain available from the Actions run; the OCI archive is retained for 7 days.
4. Tag the merged commit and push only that tag:

   ```sh
   git switch main
   git pull --ff-only
   git tag -a v0.1.1 -m 'ponydb 0.1.1'
   git push origin v0.1.1
   ```

The workflow checks the tag, lockfile, notes, and `main` ancestry, then runs
[full CI](../.github/workflows/ci.yml) against that exact commit. It builds and
tests CLI packages without publishing credentials. Both Linux release binaries
run the complete S3/HTTP/MCP suite against isolated RustFS over HTTPS; release
containers are also tested on their native CPU architectures.

After validation, the workflow uploads the CLI packages to a draft release,
records build attestations, builds a candidate OCI index in GHCR, and copies the
index and attestations to Docker Hub without rebuilding. Image verification
receives the candidate metadata as an Actions artifact using a read-only repository
token. Native runners pull both copies anonymously and check their binary hashes,
authentication, SQL, MCP, SIGTERM, and data persistence after replacing the container
and its cache. Only then does publication promote exact version tags, update
applicable aliases, and automatically make the GitHub Release public.

## Recover a failed release

If publication fails with `tag_name was used by an immutable release`, publish a
new version. GitHub permanently reserves tags used by immutable releases, even
after the release and tag are deleted; rerunning with the same version cannot
resolve this error. Update the workspace version, lockfile, and changelog, merge
to `main`, and create a new version tag as described above. See
[GitHub's immutable release rules](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases).

Open the failed Actions run and **rerun failed jobs** first. CLI build artifacts
are retained for 30 days. Once the draft contains `release-metadata.json`,
`SHA256SUMS`, and all five packages, it is the persistent recovery checkpoint.
Each publication attempt checks the current draft again, so retrying a failed
registry copy preserves the already recorded image digest and original packages.

To start a new recovery run, select the existing tag as the workflow ref, enter
the same `tag`, and disable `dry_run`. The workflow reuses the draft's original
packages and candidate digest. Already public releases are verified without
modification. Existing version tags or assets with different content cause a
failure rather than replacement.

GitHub and the two registries cannot commit atomically. A failed run may leave a
draft, candidate tags, or one promoted version tag. Alias promotion can also
partially succeed. Fix the reported registry/configuration problem and resume
the same release; the checkpoint determines the content used to complete the
remaining operations. The workflow serializes releases and does not move aliases
backward when an older maintenance release completes later.

If initial CLI upload fails before a complete checkpoint exists, rerun the failed
jobs while their build artifacts are retained. If those artifacts are lost and
existing partial assets cannot be reproduced exactly, remove only the unpublished
draft and its candidate tags before retrying, or publish a new version. Never
replace the assets or tag of a public release.

For a deployment rollback, select a previous full version or digest after checking
storage-format compatibility. Rolling back the program does not roll back S3
data. Local cache and work directories remain disposable; S3 HEAD and conditional
writes remain authoritative.
