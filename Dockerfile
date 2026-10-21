# --- Stage 1: frontend (Vite SPA, web transport mode) -----------------------
FROM node:22-bookworm-slim AS frontend
WORKDIR /app
RUN corepack enable

# Dependency manifests first for layer caching.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile --ignore-scripts

# Only the frontend inputs are needed for `build:web`.
COPY index.html vite.config.ts tsconfig.json tsconfig.node.json ./
COPY src ./src

ARG AGENTERO_BUILD_VERSION=
RUN pnpm build:web

# --- Stage 2: Rust server *and* runtime assets ------------------------------
FROM rust:bookworm AS builder
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends \
		build-essential pkg-config ca-certificates curl xz-utils \
	&& rm -rf /var/lib/apt/lists/*

# Workspace manifests + all members (cargo resolves the whole workspace).
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY cli ./cli
COPY src-tauri ./src-tauri

# Debug symbols/PGO are not needed for the server; the release profile in the
# workspace already strips + LTOs.
RUN cargo build --release -p agentero-web
# PDFium staging needs Node (the script pins the platform release).
COPY --from=frontend /usr/local/bin/node /usr/local/bin/node
# Download PDFium (liteparse dlopens it at runtime; `PDFIUM_LIB_PATH` points at
# the staged directory in the runtime image).
RUN node scripts/prepare-pdfium.mjs || true

# --- Stage 3: runtime -------------------------------------------------------
FROM debian:bookworm-slim AS runtime
ENV DEBIAN_FRONTEND=noninteractive \
	AGENTERO_WEB_ADDR=0.0.0.0:8787 \
	AGENTERO_WEB_VAULT=/vault \
	AGENTERO_WEB_DIST=/app/dist \
	XDG_CONFIG_HOME=/data/config \
	XDG_DATA_HOME=/data/data \
	XDG_CACHE_HOME=/data/cache \
	HOME=/data/home \
	RUST_LOG=info

RUN apt-get update && apt-get install -y --no-install-recommends \
		ca-certificates libstdc++6 libgcc-s1 tini \
	&& rm -rf /var/lib/apt/lists/*

# Node 22 is the runtime for bundled ACP adapters (claude/codex agent CLIs).
COPY --from=frontend /usr/local/bin/node /usr/local/bin/node
COPY --from=frontend /usr/local/lib/node_modules /usr/local/lib/node_modules

WORKDIR /app
COPY --from=builder /src/target/release/agentero-web /app/agentero-web
COPY --from=builder /src/src-tauri/pdfium /app/pdfium
COPY --from=frontend /app/dist /app/dist
ENV PDFIUM_LIB_PATH=/app/pdfium

# Bundled ACP adapter trees are staged by `pnpm adapters:stage`; mount or bake
# them under /app/adapters. The agent host resolves this path at runtime.
VOLUME ["/vault", "/data"]
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
	CMD wget -qO- http://127.0.0.1:8787/api/healthz >/dev/null 2>&1 || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/app/agentero-web"]
