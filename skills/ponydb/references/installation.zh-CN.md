# 安装指南

[English](installation.md)

安装 ponydb CLI 或容器镜像，完成平台选择、校验和源码构建。安装后，按[配置指南](configuration.zh-CN.md)连接对象存储，或按 [HTTP 指南](http.zh-CN.md)部署服务。

## 从源码安装

没有可用发布包时，安装 Rust 1.95 并在仓库根目录运行：

```sh
cargo install --locked --path apps/cli
ponydb version
```

容器可执行 `docker build -t ponydb .`，部署配置见 [HTTP 指南](http.zh-CN.md#docker)。下面的下载和镜像命令适用于相应版本已发布的情况。

## 下载 CLI

在 [GitHub Releases](https://github.com/hackerneocom/ponydb/releases) 选择版本，
下载对应压缩包及 `SHA256SUMS`：

| 系统 | 文件名中的 target | 格式 |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` | `.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | `.zip` |

以 Linux x86_64 为例，将版本号替换为所需版本：

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

请确保 `$HOME/.local/bin` 在 `PATH` 中。macOS 使用 `shasum -a 256 PACKAGE.tar.gz`
计算摘要，与 `SHA256SUMS` 中对应文件比较，然后解压安装。Windows 使用
`Get-FileHash PACKAGE.zip -Algorithm SHA256` 比较摘要，用 `Expand-Archive PACKAGE.zip`
解压，再把 `ponydb.exe` 放到用户 `PATH` 中的目录。包内附许可证、README 和
`examples/ponydb.toml`。

Linux 程序静态链接 musl、SQLite 和 TLS 依赖，不要求 glibc；访问 HTTPS 存储仍需要
系统 CA 证书。发布检查在两种架构的 Debian、Alpine 中验证 DNS 和私有测试 CA 下的
HTTPS 请求。macOS 在 macOS 15 测试，Windows 在 Windows Server 2025 测试，未据此
承诺兼容所有更旧系统。Windows 静态链接 MSVC 运行库。首版流水线不提供 Apple
公证或 Windows Authenticode 签名。

`SHA256SUMS` 校验五个 CLI 包。`release-metadata.json` 记录压缩包和程序摘要、源码
提交、Rust 版本及容器索引 digest。GitHub 中也保存构建来源证明，安装 GitHub CLI 后
可以执行：

```sh
gh attestation verify PACKAGE.tar.gz --repo hackerneocom/ponydb
```

## 容器镜像

主镜像为 `ghcr.io/hackerneocom/ponydb`，Docker Hub 镜像地址见每个 Release 的说明和
元数据。两处保存相同 OCI 索引 digest，包含 `linux/amd64`、`linux/arm64`。镜像内
的程序与对应 Linux CLI 包相同。

```sh
docker pull ghcr.io/hackerneocom/ponydb:0.1.1
docker run --rm ghcr.io/hackerneocom/ponydb:0.1.1 version
```

部署时固定完整版本或 `image@sha256:...`。正式版更新对应 `X.Y` 标签，且仅在版本
更新时推进 `latest`。`0.2.0-rc.1` 等预发布只提供完整版本标签。`sha-COMMIT` 用于
发布恢复，不作为安装渠道。

镜像默认启动 `ponydb server`，使用 UID 10001，缓存位于可丢弃的 `/cache`。提供可读
配置，并在容器内监听 `0.0.0.0`，见 [Docker 示例](http.zh-CN.md#docker)。仍可
通过 `docker build -t ponydb .` 从源码构建。
