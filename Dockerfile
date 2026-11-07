# syntax=docker/dockerfile:1
# Multi-stage build; produces an image for the build platform's CPU architecture.
FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p ponydb

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /nonexistent --shell /usr/sbin/nologin ponydb \
    && mkdir -p /cache \
    && chown ponydb:ponydb /cache
COPY --from=build /src/target/release/ponydb /usr/local/bin/ponydb
USER ponydb
# /cache and system temporary workspaces are disposable; all acknowledged data lives in S3.
ENV PONYDB_CACHE__DIRECTORY=/cache
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["ponydb"]
CMD ["server"]
