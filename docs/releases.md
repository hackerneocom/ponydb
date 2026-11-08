# Releases and installation

[中文](releases.zh-CN.md)

ponydb releases contain native CLI packages and Linux container images built from
the same tagged commit. Release automation runs in [GitHub Actions](../.github/workflows/release.yml).

## Download a CLI

Open [GitHub Releases](https://github.com/hackerneocom/ponydb/releases), choose a
version, and download its package and `SHA256SUMS`:

| System | Target in the filename | Package |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` | `.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | `.zip` |

For example, on Linux x86_64, replace the version with the release you want:

```sh
VERSION=0.1.0
PACKAGE="ponydb-$VERSION-x86_64-unknown-linux-musl"
curl --fail --location --remote-name \
  "https://github.com/hackerneocom/ponydb/releases/download/v$VERSION/$PACKAGE.tar.gz"
curl --fail --location --remote-name \
  "https://github.com/hackerneocom/ponydb/releases/download/v$VERSION/SHA256SUMS"
sha256sum --ignore-missing --check SHA256SUMS
tar -xzf "$PACKAGE.tar.gz"
mkdir -p "$HOME/.local/bin"
install -m 0755 "$PACKAGE/ponydb" "$HOME/.local/bin/ponydb"
ponydb version
```

Ensure `$HOME/.local/bin` is on your `PATH`. On macOS, compare the output of
`shasum -a 256 PACKAGE.tar.gz` with the matching filename in `SHA256SUMS`, then
extract and install the binary. On Windows, compare `Get-FileHash PACKAGE.zip
-Algorithm SHA256` with the matching entry, use `Expand-Archive PACKAGE.zip`, and
put `ponydb.exe` in a directory on your user `PATH`. Packages include the license,
READMEs, and `examples/ponydb.toml`.

Linux binaries statically link musl, bundled SQLite, and TLS dependencies; they do
not require glibc. Install your distribution's CA certificate package for HTTPS
storage endpoints. Release tests exercise both architectures on Debian and Alpine,
including DNS and HTTPS requests to a private test CA. macOS packages are tested on
macOS 15 and Windows packages on Windows Server 2025. These tests do not establish
compatibility with every older OS version. Windows links the MSVC runtime
statically. The first release pipeline does not apply Apple notarization or
Windows Authenticode signatures.

`SHA256SUMS` covers the five CLI archives. `release-metadata.json` records their
archive and executable hashes, source commit, Rust version, and container index
digest. Build attestations are recorded in GitHub; with the GitHub CLI installed,
verify a downloaded package using:

```sh
gh attestation verify PACKAGE.tar.gz --repo hackerneocom/ponydb
```

## Container images

The canonical image is `ghcr.io/hackerneocom/ponydb`; the Docker Hub mirror address
is listed in each release's notes and metadata. Both registries contain the same
OCI index digest, with `linux/amd64` and `linux/arm64` images. Each image contains
the same executable as its Linux CLI package.

```sh
docker pull ghcr.io/hackerneocom/ponydb:0.1.0
docker run --rm ghcr.io/hackerneocom/ponydb:0.1.0 version
```

Use a full version or `image@sha256:...` in deployments. Stable releases also
advance their `X.Y` alias and, when newer than the current version, `latest`.
Prereleases such as `0.2.0-rc.1` have only a full version tag. `sha-COMMIT` tags
are temporary publication checkpoints and are not a supported installation channel.

The image defaults to `ponydb server`, runs as UID 10001, and keeps disposable
cache files in `/cache`. Supply a readable configuration and bind the service to
`0.0.0.0` inside the container; see the [Docker example](../README.md#docker).
Local source builds remain available with `docker build -t ponydb .`.

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
   git tag -a v0.1.0 -m 'ponydb 0.1.0'
   git push origin v0.1.0
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
