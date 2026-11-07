# Development container

## Terminal Git prompt encoding

The container defaults to `C.UTF-8`, and Zsh sets the same locale at startup so it
overrides an incompatible encoding sent by an SSH client. The `amuse` theme's Git
branch icon requires UTF-8. With the `C` locale it fails with
`_omz_git_prompt_info:25: character not in range`.

After changing the Dockerfile, run **Dev Containers: Rebuild Container** so the
setting persists. Before rebuilding, restore the prompt in the current Zsh session:

```sh
export LANG=C.UTF-8 LC_ALL=C.UTF-8
```

## Bake file read permission during rebuild

Dev Containers writes `Dockerfile-with-features` into a temporary directory on the
host. A Compose build that uses Buildx Bake must be allowed to read that directory.
`docker-compose.yml` declares it through `additional_contexts`, so Compose passes
the matching `fs.read` grant to Bake.

The default path is `${TMPDIR:-/tmp}/devcontainercli-${USER}/container-features`,
which fits Linux and WSL. If the path in the log differs, set the real directory
in `.devcontainer/.env`:

```dotenv
DEVCONTAINER_FEATURES_CONTEXT=/tmp/devcontainercli-neo/container-features
```

Use the stable `container-features` parent directory. Do not include the version
or timestamp subdirectory. When building with Compose directly before Dev Containers
has run, create that directory first.

## Codex CLI

The image installs the Codex CLI with `npm install -g @openai/codex` and checks its
version during image build and container creation. After **Dev Containers: Rebuild
Container**, run this in the container terminal:

```sh
codex --version
codex
```

The host `~/.codex` directory is mounted at `/home/developer/.codex` so the container
can use and persist configuration, login state, and session data. The startup script
creates the directory and, when the owner does not match, changes ownership to the
container `developer` user so the state database and daemon directory are writable.

## Cursor CLI

The image installs the Cursor CLI for the `developer` user with the
[official install script](https://cursor.com/docs/cli/installation) and adds
`~/.local/bin` to the container and Zsh `PATH`. After **Dev Containers: Rebuild
Container**, run this in the container terminal:

```sh
agent --version
agent
```

The compatible command `cursor-agent` also works. The image build and container
creation both check the CLI version.

### Share a Cursor CLI login across containers

Compose mounts the host credential directory at `/home/developer/.config/cursor`,
and the container uses file credentials. Mounting the same source in several
containers shares one login. The host does not need the Cursor CLI installed. If
the directory is missing, Compose creates an empty one, and the startup script
fixes its permissions. A missing login does not block container startup.

On Linux and WSL the default source is `~/.config/cursor`. After installing the
CLI on the host, log in once:

```sh
agent login
```

If the host sets `XDG_CONFIG_HOME`, set `DEVCONTAINER_CURSOR_CONFIG_DIR` in
`.devcontainer/.env` to the absolute `$XDG_CONFIG_HOME/cursor` path.

On macOS the default login credentials live in the Keychain, which a mounted
directory cannot read. The current CLI can store credentials in files when
`AGENT_CLI_CREDENTIAL_STORE=file` is set. Run this once on the host:

```sh
AGENT_CLI_CREDENTIAL_STORE=file agent login
```

Then set the macOS file-credential directory in `.devcontainer/.env` to its
absolute path:

```dotenv
DEVCONTAINER_CURSOR_CONFIG_DIR=/Users/your-username/.cursor
```

The native Windows CLI stores file credentials in `%APPDATA%/Cursor`. When Dev
Containers is started from native Windows, set the variable to that absolute path,
for example `C:/Users/your-username/AppData/Roaming/Cursor`. When starting through
WSL, use the Linux path.

Applying a mount change requires recreating the container. A later login on the
host updates the credentials in the same directory, so the image does not need to
be rebuilt. A CLI process that is already running may cache old credentials; start
it again. Check the login with `agent status` inside the container.

You can also run `NO_OPEN_BROWSER=1 agent login` in one container and finish login
through the printed link. Other containers that mount the same directory then reuse
that login, and the host does not need the CLI installed. The shared directory can
be updated by a new login. `agent logout` in any of them clears the shared login.
The startup script may change permissions and ownership of the shared directory, so
containers that share it should use the same developer UID and GID.

Authentication is documented in the
[Cursor CLI authentication reference](https://cursor.com/docs/cli/reference/authentication).
The file-credential mode and storage path were checked against install version
`2026.10.01-e373342`. Recheck them if a newer CLI changes this behavior.

## GitHub CLI (gh)

The image installs `gh` from the
[official GitHub CLI apt repository](https://github.com/cli/cli/blob/trunk/docs/install_linux.md)
and checks its version during image build and container creation. After
**Dev Containers: Rebuild Container**, run this in the container:

```sh
gh --version
gh auth status
```

Compose mounts the host gh config directory at `/home/developer/.config/gh` and
shares its configuration and file-based credentials. Linux, macOS, and WSL default
to `~/.config/gh`, and the host `GH_CONFIG_DIR` and `XDG_CONFIG_HOME` values are
honored. To use another source, set its absolute path in `.devcontainer/.env`:

```dotenv
DEVCONTAINER_GH_CONFIG_DIR=/your/directory/gh
```

Native Windows stores the default config in `%APPDATA%/GitHub CLI`, so set the
variable explicitly, for example
`C:/Users/your-username/AppData/Roaming/GitHub CLI`. When starting through WSL,
use the Linux path. If the host has never installed or logged in to gh, Compose
creates an empty directory and the container still starts. The startup script fixes
ownership of the `.config` parent and `.config/gh`, which avoids `permission denied`
during login and allows a first `gh auth login` inside the container.

gh may store the token in the macOS Keychain, Windows Credential Manager, or a
Linux keyring. A directory mount cannot read the host keyring. To share credentials
through the directory, log in once on the host with file storage:

```sh
gh auth login --web --insecure-storage
```

That stores the token in plaintext in the shared config directory, so several
containers can reuse it. You can also set `GH_TOKEN` or `GITHUB_TOKEN` in the host
process that starts Dev Containers. Compose passes it into the container as
`GH_TOKEN`, which takes priority over file credentials. Do not write the token into
repository files or the image, and do not run a login command at container startup.

Mount or environment changes require recreating the container. With shared file
credentials, login, logout, and account switches affect every container using that
directory. Those containers should use the same developer UID and GID. See
[gh auth login](https://cli.github.com/manual/gh_auth_login) and the
[gh environment variables](https://cli.github.com/manual/gh_help_environment).

## RustFS

Compose starts the official `rustfs/rustfs:1.0.4` image, pinned to the multi-platform
digest `sha256:ed9210d6411d77ffceb70d94b964a3cb7d225983368bc25bebe01e995293dcc2`
for Linux amd64 and arm64. The embedded Console provides bucket, object, access
key, and IAM management.

RustFS starts after the development container through `depends_on` and uses
`network_mode: service:devcontainer` to share its network namespace. Inside the
development container, use `http://localhost:9000` for S3. The `rustfs` network alias
belongs to the development container, so other containers on the Compose network
can use `http://rustfs:9000`. RustFS does not publish host ports; use editor port
forwarding or an SSH tunnel to access the Console from the host.

RustFS runs in single-node single-disk mode and stores data in the `rustfs-data`
named volume mounted at `/data`. Recreating the container preserves that volume.
The health check probes both the S3 API and the Console; the development container
can start before RustFS becomes healthy.

| Purpose | Address |
| --- | --- |
| S3 API | http://localhost:9000 |
| Admin console | http://localhost:9001 |

Both the access key and the secret key are `rustfsadmin`. Sign in to the Console
with these credentials and create a bucket named `ponydb-test`, with versioning
and object retention disabled, before running the [S3 end-to-end tests](../tests/README.md).

To replace an existing MinIO deployment, run this from the repository root on the
host before reopening or rebuilding the development container:

```sh
docker compose -f .devcontainer/docker-compose.yml up -d --no-build --remove-orphans devcontainer rustfs
docker compose -f .devcontainer/docker-compose.yml ps
```

`--remove-orphans` removes the previous `ponydb_minio` service container, releasing
ports 9000 and 9001 in the shared network namespace. It preserves the old
`ponydb_minio-data` volume. RustFS starts with a separate empty volume; existing
MinIO data is not migrated. Do not use `down -v` or remove the old volume if you
need to retain its data. Update local S3 credentials to `rustfsadmin`; the
`localhost:9000` endpoint stays the same.

## SSH login

After changing this configuration, run **Dev Containers: Rebuild Container** in
VS Code. sshd starts with the container and stays in the foreground. If it exits
unexpectedly, Compose `restart: unless-stopped` restarts the container. Stopping
the container on purpose does not start it again.

From the repository root on the host, stop the development container with:

```sh
docker compose -f .devcontainer/docker-compose.yml stop
```

That matches closing the window when `shutdownAction` is `stopCompose` in
`devcontainer.json`: the container stops, and the container, network, and volumes
remain. Reopening the development container, or running
`docker compose -f .devcontainer/docker-compose.yml start`, starts the same
container.

Host port `22328` is published to container port `22` and stays the same across
restarts. Login does not require a public key or a password:

```sh
ssh -p 22328 developer@<host-ip>
```

Set the host port and bind address in `.devcontainer/.env`:

```dotenv
DEVCONTAINER_SSH_PORT=22323
DEVCONTAINER_SSH_BIND_ADDRESS=127.0.0.1
```

These variables choose a fixed port for each host. Startup does not search for a
free port. If the port is in use or reserved, pick another one. Applying the new
port requires recreating the container (**Dev Containers: Rebuild Container**).
Restarting the existing container does not apply the new port.

The port is published on all host interfaces by default. Anyone who can reach it
can log in as `developer` and use this container's passwordless sudo. The container
also mounts the host `~/.ssh` directory. For local-only use, bind it to `127.0.0.1`.

After the container starts, confirm the mapping from the repository root:

```sh
docker compose -f .devcontainer/docker-compose.yml port devcontainer 22
```

External access also requires the host network and firewall to allow the port. The
SSH port is published by Compose and does not depend on VS Code port forwarding.

SSH host keys are stored in their own Compose volume and stay the same when the
container is rebuilt. Deleting that volume generates new host keys, and clients
must accept the new host fingerprint.

The authentication mechanism is documented in the
[OpenSSH sshd_config manual](https://man.openbsd.org/sshd_config#AuthenticationMethods).

### Choosing a port on Windows and macOS

TCP listen ports can be `1–65535`, but avoid well-known services, the privileged
range `1–1023`, and ports this machine already uses or reserves. The default
dynamic port range on both Windows and macOS is `49152–65535`, used when the system
assigns a connection port automatically. That range can be changed, so check the
local machine. Sources:
[Microsoft documentation](https://learn.microsoft.com/en-us/troubleshoot/windows-client/networking/tcp-ip-port-exhaustion-troubleshooting)
and the
[Apple XNU default configuration](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/in_pcb.c).

Prefer a port in `22306–22332`, such as this project's default `22328`. That range
sits outside the default dynamic range on both systems and is currently unassigned
in the
[IANA registry](https://www.iana.org/assignments/service-names-port-numbers?search=2233).
It only reduces the chance of a conflict. It does not guarantee the port is free
on this machine.

Query the dynamic range, reserved ranges, and current listeners in Windows
PowerShell:

```powershell
netsh int ipv4 show dynamicport tcp
netsh int ipv6 show dynamicport tcp
netsh int ipv4 show excludedportrange protocol=tcp
netsh int ipv6 show excludedportrange protocol=tcp
Get-NetTCPConnection -State Listen -LocalPort 22328 -ErrorAction SilentlyContinue
```

Query the dynamic range and current listener on macOS:

```sh
sysctl net.inet.ip.portrange.first net.inet.ip.portrange.last
lsof -nP -iTCP:22328 -sTCP:LISTEN
```
