# Two images from one file:
#   --target orchestrator  the queue, the dashboard, and the GitHub App key. Starts jobs with Docker.
#   --target job           runs one rebase job: untrusted repo code and an agent. No GitHub write access.

FROM node:24-bookworm-slim AS ui
WORKDIR /src/ui
RUN corepack enable
COPY ui/package.json ui/pnpm-lock.yaml ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY ui/ ./
RUN pnpm build

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
COPY --from=ui /src/ui/dist ui/dist
RUN cargo build --release --locked -p mergepatrol

FROM debian:bookworm-slim AS orchestrator
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=docker:cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=build /src/target/release/mergepatrol /usr/local/bin/mergepatrol
ENV MERGEPATROL_DATA_DIR=/data MERGEPATROL_BIND=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
# Root, to use the host's Docker socket: the orchestrator is trusted, the jobs are not.
ENTRYPOINT ["mergepatrol"]

FROM node:24-bookworm-slim AS job
# The default job image: git, the Claude CLI, and common toolchains for verify commands.
RUN apt-get update && apt-get install -y --no-install-recommends \
        git ca-certificates build-essential pkg-config python3 python3-pip python3-venv \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @anthropic-ai/claude-code \
    && corepack enable
COPY --from=golang:1-bookworm /usr/local/go /usr/local/go
COPY --from=rust:1-slim-bookworm /usr/local/rustup /usr/local/rustup
COPY --from=rust:1-slim-bookworm /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/home/node/.cargo \
    PATH=/usr/local/cargo/bin:/usr/local/go/bin:/home/node/go/bin:$PATH
COPY --from=build /src/target/release/mergepatrol /usr/local/bin/mergepatrol
RUN mkdir /out && chown node /out
USER node
WORKDIR /home/node
ENTRYPOINT ["mergepatrol"]
