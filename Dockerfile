FROM node:24-bookworm-slim AS ui
WORKDIR /src/ui
RUN corepack enable
COPY ui/package.json ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY ui/ ./
RUN pnpm build

FROM rust:1-slim-bookworm AS app
WORKDIR /src
COPY . .
COPY --from=ui /src/ui/dist ui/dist
RUN cargo build --release --locked -p mergepatrol

FROM node:24-bookworm-slim
# git for the rebases, the Claude CLI for the resolver.
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @anthropic-ai/claude-code
COPY --from=app /src/target/release/mergepatrol /usr/local/bin/mergepatrol
ENV MERGEPATROL_DATA_DIR=/data MERGEPATROL_BIND=0.0.0.0:8080
RUN mkdir /data && chown node /data
VOLUME /data
EXPOSE 8080
USER node
ENTRYPOINT ["mergepatrol"]
