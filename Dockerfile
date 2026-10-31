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

# Pre-stage the runtime inputs so the cargo build never depends on flaky
# GitHub downloads:
#  - PDFium (liteparse's build script downloads it into ~/.cache/pdfium-rs);
#  - tesseract/leptonica sources (tesseract-rs otherwise downloads them).
COPY --from=frontend /usr/local/bin/node /usr/local/bin/node
RUN node scripts/prepare-pdfium.mjs \
    || (sleep 3 && node scripts/prepare-pdfium.mjs) \
    || (mkdir -p src-tauri/pdfium && echo "WARN: pdfium staging failed")
RUN mkdir -p /root/.tesseract-rs/third_party /tmp/tess \
    && curl -fsSL --retry 3 -o /tmp/tess/leptonica.zip \
        https://github.com/DanBloomberg/leptonica/archive/refs/tags/1.84.1.zip \
    && curl -fsSL --retry 3 -o /tmp/tess/tesseract.zip \
        https://github.com/tesseract-ocr/tesseract/archive/refs/tags/5.3.4.zip \
    && unzip -q /tmp/tess/leptonica.zip -d /tmp/tess/lp \
    && unzip -q /tmp/tess/tesseract.zip -d /tmp/tess/tc \
    && mv /tmp/tess/lp/leptonica-1.84.1 /root/.tesseract-rs/third_party/leptonica \
    && mv /tmp/tess/tc/tesseract-5.3.4 /root/.tesseract-rs/third_party/tesseract \
    && rm -rf /tmp/tess

# Debug symbols/PGO are not needed for the server; the release profile in the
# workspace already strips + LTOs.
RUN cargo build --release -p agentero-web

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

# Dsh's workspace-write sandbox needs bubblewrap + namespace privileges in
# this container. Compose deployments that cannot grant those (the default
# Docker seccomp profile blocks bwrap namespace creation) run the agent with
# its supported danger-full-access preset inside this single-user container.
# Override DSH_PERMISSION_MODE=workspace-write only together with the
# cap_add/seccomp settings documented in docker-compose.yml.example.
ENV DSH_PERMISSION_MODE=danger-full-access

RUN apt-get update && apt-get install -y --no-install-recommends \
		ca-certificates libstdc++6 libgcc-s1 tini bubblewrap \
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
