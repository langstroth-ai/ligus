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
# worktrees by shelling out to git), CA certificates, bubblewrap, and tini as
# PID 1 so subprocesses an agent turn starts (git, CLI agents, MCP stdio
# servers) are reaped.
#
# The agent sandbox: bubblewrap needs unprivileged user namespaces, which a
# Kubernetes Pod Security "restricted" pod (RuntimeDefault seccomp, no
# capabilities) does not give it. There Ligus uses Landlock instead, which
# needs Linux 6.7+ with landlock in the node's LSM list and a runtime seccomp
# profile that allows the landlock_* syscalls. With no confinement at all
# Ligus refuses every agent turn that runs a local program (CLI agents,
# run_command, stdio MCP servers); `ligus doctor --require-confined
# --turn-test` checks a node (the Helm chart runs it as an init container). Agent CLIs (claude, codex, ...) are NOT included: extend this image
# with the ones you use. See docs/design/kubernetes.md.

# Base images are pinned by tag and index digest (multi-arch); bump the
# version ARGs and both digests together.
ARG RUST_VERSION=1.91
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE}@sha256:c1e5f19e773b7878c3f7a805dd00a495e747acbdc76fb2337a4ebf0418896b33 AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p hive-cli \
 && install -Dm755 target/release/ligus /out/ligus

FROM debian:${DEBIAN_RELEASE}-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN apt-get update \
 && apt-get install -y --no-install-recommends bubblewrap ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 65532 ligus \
 && useradd --system --uid 65532 --gid 65532 --home-dir /var/lib/ligus --shell /usr/sbin/nologin ligus \
 && install -d -o 65532 -g 65532 -m 0700 /var/lib/ligus /home/ligus
COPY --from=build /out/ligus /usr/local/bin/ligus
# Identity, device keys, hive.db, serve.token and checkouts live in the data
# dir. Mount a persistent volume there; it holds secrets, keep it private
# (0700). HOME (agent CLIs' logins and caches) and the working directory are
# deliberately elsewhere: an agent process never runs in, or reads, the data
# dir.
ENV LIGUS_DATA_DIR=/var/lib/ligus \
    HOME=/home/ligus
USER 65532:65532
WORKDIR /home/ligus
EXPOSE 8787
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/ligus"]
CMD ["--version"]
