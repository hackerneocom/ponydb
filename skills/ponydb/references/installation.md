# Installation guide

[中文](installation.zh-CN.md)

Install the ponydb CLI or container image, choose a platform, verify downloads, or build from source. After installation, follow [configuration](configuration.md) to connect to object storage, or [HTTP deployment](http.md) to run a service.

## Build from source

If no release package is available, install Rust 1.95 and run from the repository root:

```sh
cargo install --locked --path apps/cli
ponydb version
```

For a container, run `docker build -t ponydb .` and follow [HTTP deployment](http.md#docker). The download and image commands below apply once the selected version has been published.

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
VERSION=0.1.1
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
docker pull ghcr.io/hackerneocom/ponydb:0.1.1
docker run --rm ghcr.io/hackerneocom/ponydb:0.1.1 version
```

Use a full version or `image@sha256:...` in deployments. Stable releases also
advance their `X.Y` alias and, when newer than the current version, `latest`.
Prereleases such as `0.2.0-rc.1` have only a full version tag. `sha-COMMIT` tags
are temporary publication checkpoints and are not a supported installation channel.

The image defaults to `ponydb server`, runs as UID 10001, and keeps disposable
cache files in `/cache`. Supply a readable configuration and bind the service to
`0.0.0.0` inside the container; see the [Docker example](http.md#docker).
Local source builds remain available with `docker build -t ponydb .`.
