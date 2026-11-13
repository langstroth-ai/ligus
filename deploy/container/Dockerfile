# syntax=docker/dockerfile:1
#
# Container image for the headless `ligus` binary (hive-cli): `ligus serve`,
# `ligus worker`, `ligus enroll`, `ligus sync`, ... It is not the desktop app and
# not the relay (the relay has its own image in github.com/ligus-ai/relay).
#
# Build from the repository root:
#   docker build -f deploy/container/Dockerfile -t ligus:dev .
#   docker run --rm ligus:dev --version
#
# Multi-arch (linux/amd64 + linux/arm64) with buildx:
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     -f deploy/container/Dockerfile -t <registry>/ligus:<version> --push .
#
# Runtime contents: the ligus binary, git (agent turns cut per-turn git
# worktrees by shelling out to git), CA certificates, and tini as PID 1 so
# subprocesses an agent turn starts (git, CLI agents, MCP stdio servers) are
# reaped. Agent CLIs (claude, codex, ...) are NOT included: extend this image
# with the ones you use. See docs/design/kubernetes.md.

ARG RUST_VERSION=1.91
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p hive-cli \
 && install -Dm755 target/release/ligus /out/ligus

FROM debian:${DEBIAN_RELEASE}-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 65532 ligus \
 && useradd --system --uid 65532 --gid 65532 --home-dir /var/lib/ligus --shell /usr/sbin/nologin ligus \
 && install -d -o 65532 -g 65532 -m 0700 /var/lib/ligus
COPY --from=build /out/ligus /usr/local/bin/ligus
# Identity, device keys, hive.db, serve.token and checkouts live here. Mount a
# persistent volume at this path; it holds secrets, keep it private (0700).
ENV LIGUS_DATA_DIR=/var/lib/ligus \
    HOME=/var/lib/ligus
USER 65532:65532
WORKDIR /var/lib/ligus
EXPOSE 8787
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/ligus"]
CMD ["--version"]
