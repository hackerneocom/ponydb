# Bullseye's security package downloads return 404 after its LTS end of life.
FROM rust:1.95.0-bookworm

ARG USERNAME=developer
ARG USER_UID=1000
ARG USER_GID=$USER_UID
ARG NODE_MAJOR=24

# Layer 1: non-root user and sudo (rarely changes)
RUN groupadd --gid $USER_GID $USERNAME \
    && useradd --uid $USER_UID --gid $USER_GID -m $USERNAME \
    && apt-get update \
    && apt-get install -y --no-install-recommends sudo \
    && echo "$USERNAME ALL=(root) NOPASSWD:ALL" > /etc/sudoers.d/$USERNAME \
    && chmod 0440 /etc/sudoers.d/$USERNAME \
    && rm -rf /var/lib/apt/lists/*

# Layer 2: base packages, locale, and timezone (rarely changes)
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gnupg \
        git \
        wget \
        zsh \
        vim \
        locales \
        fonts-powerline \
        openssh-server \
    && sed -i '/en_US.UTF-8/s/^# //g' /etc/locale.gen \
    && locale-gen \
    && ln -fs /usr/share/zoneinfo/Asia/Shanghai /etc/localtime \
    && dpkg-reconfigure -f noninteractive tzdata \
    # Wrap curl to force TLS 1.2 and avoid libcurl TLS 1.3 handshake failures (SSL_ERROR_SYSCALL) on constrained Docker networks.
    && mv /usr/bin/curl /usr/bin/curl.real \
    && printf '#!/bin/sh\nexec /usr/bin/curl.real --tls-max 1.2 "$@"\n' > /usr/bin/curl \
    && chmod +x /usr/bin/curl \
    && rm -f /etc/ssh/ssh_host_* \
    && rm -rf /var/lib/apt/lists/*

# Set the locale before Oh My Zsh loads, including SSH client overrides.
RUN printf '\n# UTF-8 is required by the amuse Git prompt.\nexport LANG=C.UTF-8\nexport LC_ALL=C.UTF-8\nexport PATH="$HOME/.local/bin:$PATH"\n' >> /etc/zsh/zshenv

# Host keys are generated at runtime and persisted in a Compose volume.
COPY sshd_config /etc/ssh/sshd_config_devcontainer
COPY start.sh /usr/local/bin/start-devcontainer
RUN chmod 0755 /usr/local/bin/start-devcontainer

# Layer 3: Node.js 24 (NodeSource apt repository)
RUN wget -qO- "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && chown -R ${USERNAME}:${USERNAME} /usr/local \
    && npm config set prefix /usr/local \
    && node --version \
    && npm --version \
    && rm -rf /var/lib/apt/lists/*

# Layer 4: Claude Code CLI (depends on Node.js; separate layer for caching)
RUN npm install -g @anthropic-ai/claude-code \
    && claude --version

# Codex CLI (depends on Node.js; separate layer for caching)
RUN npm install -g @openai/codex \
    && codex --version

# GitHub CLI (official apt repository)
RUN install -d -m 0755 /etc/apt/keyrings \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
        -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && chmod 0644 /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && printf 'deb [arch=%s signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\n' "$(dpkg --print-architecture)" > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends gh \
    && gh --version \
    && rm -rf /var/lib/apt/lists/*

USER $USERNAME
WORKDIR /home/$USERNAME

ENV PATH="/home/${USERNAME}/.local/bin:${PATH}"

# Cursor CLI (official install script, installed as the developer user so it can auto-update)
RUN bash -o pipefail -c 'curl -fsSL https://cursor.com/install | bash' \
    && agent --version \
    && cursor-agent --version

# Layer 5: Oh My Zsh and plugins (separate layer)
RUN sh -c "$(wget -qO- https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)" "" --unattended \
    && test -f "$HOME/.oh-my-zsh/oh-my-zsh.sh" \
    && git clone --depth=1 https://github.com/zsh-users/zsh-syntax-highlighting.git \
        "${ZSH_CUSTOM:-$HOME/.oh-my-zsh/custom}/plugins/zsh-syntax-highlighting" \
    && git clone --depth=1 https://github.com/zsh-users/zsh-autosuggestions.git \
        "${ZSH_CUSTOM:-$HOME/.oh-my-zsh/custom}/plugins/zsh-autosuggestions" \
    && sed -i 's/^ZSH_THEME=.*/ZSH_THEME="amuse"/' ~/.zshrc \
    && sed -i 's/^plugins=.*/plugins=(git zsh-syntax-highlighting zsh-autosuggestions)/' ~/.zshrc \
    && printf '%s\n' \
        'export HISTFILE=~/zsh_history/.zsh_history' \
        'setopt INC_APPEND_HISTORY' \
        'setopt SHARE_HISTORY' \
        >> ~/.zshrc \
    && printf '%s\n' \
        'set fileencodings=utf-8,ucs-bom,gb18030,gbk,gb2312,cp936' \
        'set termencoding=utf-8' \
        'set encoding=utf-8' \
        >> ~/.vimrc \
    && mkdir -p ~/zsh_history

# Layer 6: Rust development components (changes occasionally)
RUN rustup component add --toolchain 1.95.0 clippy rustfmt

# Layer 7: cargo-watch (slow to compile; separate layer)
RUN cargo install cargo-watch

ENV SHELL=/bin/zsh \
    TZ=Asia/Shanghai \
    GIT_EDITOR=vim \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8
