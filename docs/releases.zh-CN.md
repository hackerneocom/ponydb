# 发布与安装

[English](releases.md)

ponydb 从同一版本 tag 发布原生 CLI 包和 Linux 容器镜像，自动化入口是
[GitHub Actions](../.github/workflows/release.yml)。

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
配置，并在容器内监听 `0.0.0.0`，见 [Docker 示例](../README.zh-CN.md#docker)。仍可
通过 `docker build -t ponydb .` 从源码构建。

## 维护者首次配置

1. 在 Docker Hub 创建公开的 `ponydb` 仓库，把其用户或组织名设为 GitHub 仓库变量
   `DOCKERHUB_NAMESPACE`。
2. 创建名为 `release` 的 GitHub Environment，配置环境变量 `DOCKERHUB_USERNAME`
   和环境 Secret `DOCKERHUB_TOKEN`。令牌需要目标 Docker Hub 仓库写权限。工作流
   本身不要求人工审批，请相应配置该环境。
3. 允许 `release` 环境从 `v*` tag 发布。手工恢复时，在 Actions 的 **Run workflow**
   引用选择器中选择同一 tag，并在 `tag` 输入中填写它。
4. 为 `main` 和 `v*` 设置保护规则，仅允许维护者或发布自动化创建 tag，禁止删除或
   强制更新版本 tag，并启用 GitHub 不可变 Release。
5. 允许草稿查询任务写入仓库内容，允许发布任务写入仓库内容、Packages 和
   Attestations。GHCR 使用工作流内置的 `GITHUB_TOKEN`，不需要额外 PAT。
6. 将 GHCR 的 `ponydb` 包设为公开。即使源码仓库公开，首次创建的包仍可能是私有。
   若尚无此包，首次 RC 会创建候选镜像和 Release 草稿，在匿名拉取检查处停止。
   将包设为公开后，重跑失败任务即可。

工作流及复用的 CI 固定 Actions 提交，运行时 Debian 基础镜像和冒烟测试镜像固定
索引 digest。获取系统修复时应更新这些引用，并以新的 ponydb 版本构建，保留现有
完整版本标签不变。

## 发布版本

1. 修改根 `Cargo.toml` 的 `workspace.package.version`，同步刷新 `Cargo.lock` 中
   工作区包版本，在 [CHANGELOG.md](../CHANGELOG.md) 增加对应 `## X.Y.Z`。预发布
   同样需要精确标题，例如 `## 0.2.0-rc.1`。
2. 在说明中记录配置变化、存储格式兼容性、升级步骤和回退限制。行为变化时同步
   更新中英文文档，通过普通 CI 后合并到 `main`。
3. 首次发布或修改发布工具后，先手工启动 **Release** 并保持 `dry_run` 开启。
   它构建五个平台的 CLI，测试两种 Linux 架构，导出多架构 OCI 包，不创建发布或
   推送镜像，不需要仓库凭据。产物可从 Actions 下载；OCI 包保留 7 天。
4. 为合并后的提交创建 annotated tag，并单独推送：

   ```sh
   git switch main
   git pull --ff-only
   git tag -a v0.1.1 -m 'ponydb 0.1.1'
   git push origin v0.1.1
   ```

工作流检查 tag、锁文件、说明以及提交是否属于 `main` 历史，对该确切提交运行
[完整 CI](../.github/workflows/ci.yml)。构建任务不接触发布凭据。两种 Linux 发布
程序使用 HTTPS 连接隔离的 RustFS，运行完整 S3、HTTP、MCP 测试；容器也在对应
原生架构运行验证。

通过验证后，工作流将 CLI 包暂存到 Release 草稿，记录构建来源证明，在 GHCR 构建
候选 OCI 索引，再把索引及证明复制到 Docker Hub，不重复构建。镜像验证通过 Actions
产物接收候选元数据，任务仅需仓库读取权限。原生 runner 从两处匿名拉取，校验程序
摘要、认证、SQL、MCP、SIGTERM，以及替换容器和缓存后的数据持久性。全部通过后，
才提升完整版本标签、更新适用的浮动标签并自动公开 Release。

## 失败恢复

若发布报错 `tag_name was used by an immutable release`，必须改发新版本。GitHub
会永久保留不可变 Release 使用过的标签名，即使删除 Release 和 tag 也无法复用；
重跑同一版本不能解决此错误。按上述发布步骤更新工作区版本、锁文件和更新日志，
合并到 `main` 后创建新的版本标签。参见
[GitHub 不可变 Release 规则](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)。

优先在失败的 Actions 运行中选择 **rerun failed jobs**。CLI 构建产物保留 30 天。
草稿包含 `release-metadata.json`、`SHA256SUMS` 和五个 CLI 包后，即成为持久恢复
检查点。每次发布尝试都会重新检查当前草稿，因此镜像复制失败后重跑会保留已记录的
镜像 digest 和原始 CLI 包。

重新启动恢复运行时，选择现有 tag 作为工作流引用，在 `tag` 中填写同一 tag，关闭
`dry_run`。工作流复用草稿中的原始 CLI 包和候选 digest。已公开的版本只校验，不
修改。若已有完整版本标签或文件内容不同，则停止而不覆盖。

GitHub 与两个镜像仓库无法原子提交。失败可能留下草稿、候选标签或已提升的某个
版本标签；浮动标签提升也可能部分成功。修复日志指出的仓库或配置问题，再恢复
同一发布，工作流按检查点完成剩余步骤。发布串行执行，较旧维护版本不会把浮动
标签回退。

若首次 CLI 上传在完整检查点形成前失败，应在构建产物仍保留时重跑失败任务。
若产物已丢失且无法精确重建部分上传的文件，可只删除尚未公开的草稿及其候选标签
后重试，或改发新版本。不要替换已公开版本的文件或 tag。

部署回退应在确认存储格式兼容后选择旧完整版本或 digest。回退程序不回退 S3 数据；
本地缓存和工作目录仍可丢弃，S3 HEAD 及条件写入仍是权威数据来源。
