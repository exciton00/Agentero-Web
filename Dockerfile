# --- Stage 1: frontend (Vite SPA, web transport mode) -----------------------
FROM node:22-bookworm-slim AS frontend
WORKDIR /app
RUN corepack enable

# Dependency manifests first for layer caching.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
# patchedDependencies in pnpm-workspace.yaml reference this directory.
COPY patches ./patches
RUN pnpm install --frozen-lockfile --ignore-scripts

# Only the frontend inputs are needed for `build:web`.
COPY index.html vite.config.ts tsconfig.json tsconfig.node.json ./
COPY src ./src

ARG AGENTERO_BUILD_VERSION=
RUN pnpm build:web

# --- Stage 2: Rust server *and* runtime assets ------------------------------
FROM rust:bookworm AS builder
WORKDIR /src
# tesseract-rs (liteparse OCR dependency) builds tesseract+leptonica from
# source at compile time, so cmake/unzip and a C++ toolchain are required.
RUN apt-get update && apt-get install -y --no-install-recommends \
		build-essential pkg-config ca-certificates curl xz-utils cmake unzip \
	&& rm -rf /var/lib/apt/lists/*

# Workspace manifests + all members (cargo resolves the whole workspace).
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY cli ./cli
COPY src-tauri ./src-tauri
# agentero-core's build.rs embeds the Vault onboarding templates.
COPY templates ./templates
# Release helper scripts (PDFium staging; host-triple detection needs rustc).
COPY scripts ./scripts

# Debug symbols/PGO are not needed for the server; the release profile in the
# workspace already strips + LTOs.
RUN cargo build --release -p agentero-web
# PDFium staging needs Node (the script pins the platform release).
COPY --from=frontend /usr/local/bin/node /usr/local/bin/node
# Download PDFium (liteparse dlopens it at runtime; `PDFIUM_LIB_PATH` points at
# the staged directory in the runtime image). The directory must exist even if
# the download fails so the runtime stage's COPY succeeds.
RUN node scripts/prepare-pdfium.mjs && ls -la src-tauri/pdfium \
    || (echo "WARN: pdfium staging failed; PDF text parsing will be unavailable" && mkdir -p src-tauri/pdfium)

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
# Node ships in the image (ACP adapters), so use it for the probe instead of
# adding wget/curl to the runtime layer.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
	CMD node -e "fetch('http://127.0.0.1:8787/api/healthz').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"

ENTRYPOINT ["/usr/bin/tini", "--", "/app/agentero-web"]
