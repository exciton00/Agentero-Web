<p align="center">
  <img src="docs/assets/hero.png" alt="Agentero-Web" width="100%" />
</p>

<h2 align="center">Agentero-Web</h2>

<p align="center">
  A self-hostable research workbench: run it on a NAS / VPS / mini PC and use the full Agentero workspace from a browser.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
</p>

<p align="center">
  <b>English</b> | <a href="README.md">中文</a>
</p>

---

## What it is

Agentero-Web brings Agentero's research workbench to the browser. Papers, PDFs, notes and annotations live in your own **Vault** (a plain folder); the app runs on an always-on machine and you open the same workspace from a laptop, tablet or phone.

- Single-user and self-hosted; data and credentials stay on your machine.
- **No built-in auth or reverse proxy**: it listens on `127.0.0.1` by default. Add TLS / auth / a reverse proxy yourself for remote access.
- Vault Markdown, PDFs and `marks/` remain readable by any editor, shell or Git workflow outside the app.

## Features

- **Browser workbench**: file tree, paper library, PDF reader, Markdown / NOTES editor, multi-panel workspace and multi-tab windows.
- **Library & import**: browse/filter by folder and tags; import via DOI / arXiv / BibTeX / local PDF; bulk actions, replace PDF, re-parse.
- **PDF reading & annotations**: highlights with color switching, margin notes; the right-hand **Annotations** panel groups "Highlights & notes / Conversations / Visual annotations" with jump, expand-note and delete. Annotations live in the paper's `marks/` folder and never modify the original PDF.
- **Translation**: selection translation and full-document layout translation; **Settings → Translate** lets you adjust body-text and heading font sizes (live, percentage) and reset to defaults.
- **Layout & body parsing**: connect self-hosted MinerU and other remote layout/body engines to produce `layout.json`, `PAPER.md` and `images/`; set a parsing page cap.
- **Agent (ACP)**: chat with an Agent CLI installed on the host (Dsh is supported today), with paper and selection context; per-tab event routing.
- **MCP server**: expose Vault papers/notes/files to external clients over Streamable HTTP MCP; tools and paper text are opt-in.
- **External knowledge-base RAG**: sync papers as Markdown into external knowledge bases such as WeKnora, and expose read-only retrieval tools to MCP clients and the web agent.
- **Sync & citations**: Vault cloud sync (git / S3 / WebDAV, optionally automatic); live BibTeX / BibLaTeX mount at `/agentero_online.bib`.

## Quick start (Docker Compose)

Requirements: Linux + the Docker Compose plugin.

```bash
git clone https://github.com/exciton00/Agentero-Web.git agentero-web
cd agentero-web

cp docker-compose.yml.example docker-compose.yml
cp .env.example .env
# edit .env: Vault / data paths, port, host Dsh paths, optional MCP, etc.
mkdir -p vault data

docker compose pull
docker compose up -d
# open http://127.0.0.1:8787
```

- The repo ships one `docker-compose.yml.example` plus `.env.example`; the copied working files are not committed. No reverse proxy or production orchestration is included.
- The published image `ghcr.io/exciton00/agentero-web:latest` is used by default. To build it yourself instead, point `AGENTERO_IMAGE` in `.env` at a local tag and run `docker compose up -d --build`.
- The first visit shows a welcome page; create or open a Vault to start.

### Dsh agent (mounted from the host)

The image does **not** bundle an agent CLI. To use the Chat panel, use a Dsh that is already installed and logged in on the host:

```dotenv
# .env (example: root + system npm install)
AGENTERO_HOST_DSH_ROOT=/usr/lib/node_modules/@deepseek-ai
AGENTERO_DSH_HOME=/root/.dsh
```

Compose mounts these to `/opt/host-agent/@deepseek-ai` (read-only) and `/data/home/.dsh`; the web host auto-registers the normal `Dsh` catalog row at startup.

- For nvm / volta installs: run `readlink -f "$(command -v dsh)"`, strip the trailing `/dsh/lib/bin.js`, and use the remaining directory as `AGENTERO_HOST_DSH_ROOT`; set `AGENTERO_DSH_HOME` to the absolute `~/.dsh` path.
- If a `./data` volume reused from an older version hides Dsh, open **Settings → Agent → "Detect host Dsh" → "Redetect"** to clear the stale marker and register again.
- Other ACP agents must be installed and logged in yourself; the web host reaches them through the same mounts/environment.

**Vault path parity**: the container Vault defaults to `/vault`. If host tools need exactly the same absolute path inside and outside the container, set both in `.env`:

```dotenv
AGENTERO_VAULT_DIR=/srv/agentero/vault
AGENTERO_VAULT_CONTAINER_PATH=/srv/agentero/vault
```

### Common environment variables

| Variable | Default | Purpose |
|---|---|---|
| `AGENTERO_IMAGE` | `ghcr.io/exciton00/agentero-web:latest` | image to run |
| `AGENTERO_VAULT_DIR` / `AGENTERO_DATA_DIR` | `./vault` / `./data` | host Vault / app data (XDG) directories |
| `AGENTERO_VAULT_CONTAINER_PATH` | `/vault` | Vault path inside the container |
| `AGENTERO_WEB_BIND` / `AGENTERO_WEB_PORT` | `127.0.0.1` / `8787` | host bind address and port |
| `AGENTERO_HOST_DSH_ROOT` | `/usr/lib/node_modules/@deepseek-ai` | host Dsh package root (mounted into the container) |
| `AGENTERO_DSH_HOME` | `${HOME}/.dsh` | host Dsh credentials / profile directory |
| `AGENTERO_MCP_ENABLED` / `AGENTERO_MCP_BIND` / `AGENTERO_MCP_ALLOWED_HOSTS` | empty | force MCP on / listen address / `Host` allow-list |
| `AGENTERO_WEB_MAX_BODY_MB` | `64` | `/api` body limit (8–512 MiB) |
| `AGENTERO_WEB_ALLOW_ROOTS` | empty | extra roots for the file API |
| `AGENTERO_SYNC_ACCESS_KEY` / `AGENTERO_SYNC_SECRET_KEY` | empty | optional cloud-sync credentials |
| `DEEPSEEK_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | empty | optional server-side agent CLI credentials |
| `RUST_LOG` | `info` | log level |

See [`.env.example`](.env.example) for the full template.

## Data & backup

| Content | Location |
|---|---|
| Vault (Markdown / PDF / `marks/` / `.agentero/catalog.sqlite`) | host `AGENTERO_VAULT_DIR` |
| Settings, sync / RAG state, usage records, cache | host `AGENTERO_DATA_DIR` (XDG `/data` in the container) |

Backup = copy the Vault directory plus the data directory. The Vault stays readable by external tools; back it up before upgrading the image.

## Common settings

- **Settings → Agent**: detect host Dsh; use "Redetect" when Dsh is missing or an old data volume is reused.
- **Settings → Translate**: pick a translation service; adjust full-document body / heading font sizes and reset to defaults.
- **Settings → Layout**: choose layout-analysis and body-parse engines (e.g. self-hosted MinerU) and set the parsing page cap.
- **Settings → Sync**: git / S3 / WebDAV cloud sync; optional automatic sync.
- **Settings → General → MCP server**: enable/disable the MCP listener port.
- **Settings → RAG / External knowledge base**: configure an external knowledge base (see below).

## MCP clients

Enable **Settings → General → MCP server** to serve Streamable HTTP MCP on its own port (default):

```
http://127.0.0.1:8765/mcp
```

- Local clients (MCP Inspector, Codex, …) can connect directly. The in-app Agent panel uses ACP, not MCP.
- For containerized / remote clients: set `AGENTERO_MCP_ENABLED=1`, `AGENTERO_MCP_BIND=0.0.0.0:8765`, publish port 8765, and add the external host to `AGENTERO_MCP_ALLOWED_HOSTS`.
- MCP has **no built-in auth**; put it behind a reverse proxy with auth when exposed.
- Paper text is not returned by default; enable "Expose paper text" in **Settings → General** to allow it.
- Tools, protocol and security boundaries: [MCP tool docs](docs/backend/mcp.md) (advanced).

## External knowledge-base RAG

Sync Vault papers into an external knowledge base for read-only semantic retrieval by MCP clients and the web agent. **WeKnora** is supported today (RAGFlow is reserved and shown as "coming soon").

1. Create and initialize a knowledge base (document type) in WeKnora and note its **Knowledge Base ID**.
2. Create an API key with **`retrieve` + `ingest`** permissions (no KB management, chat or full access required). If the key form supports a KB allow-list, select only that KB.
3. In **Settings → RAG / External knowledge base**: choose Provider `WeKnora`, enter the Base URL (e.g. `https://<host>/api/v1`), API key and KB ID, then **Test connection**.
4. Enable **Enable RAG**, pick what to sync (body / notes / annotations / figure index / BibTeX citations) and click **Sync now**; auto-sync is optional.

Notes:

- Only text is uploaded: body, notes, annotation text, figure index and BibTeX citations — **no PDF/image binaries**.
- The API key is stored server-side only (`settings.json`, 0600); the UI shows a mask and logs/sync state never contain it.
- One Vault maps to one knowledge base; changing the KB rebuilds the mapping. Enable the tool switches if external MCP clients or the web agent should use RAG.
- Other providers (e.g. RAGFlow) are not implemented. See [RAG docs](docs/backend/rag.md) (advanced).

## Remote access & security

- It listens on `127.0.0.1` by default — fine for local or NAS LAN access.
- For internet or cross-network access, terminate TLS and add authentication in your own reverse proxy; the proxy must pass WebSocket `/api/events` through, otherwise the UI receives no live events.
- MCP has no built-in auth either; do not expose it directly. Enable it only when needed and restrict `Host` with `AGENTERO_MCP_ALLOWED_HOSTS`.
- The file API only allows the Vault and data directories by default; allow other paths explicitly with `AGENTERO_WEB_ALLOW_ROOTS`.

## Known limits

- Single-user design: no multi-tenancy, permission isolation or concurrent multi-Vault writes.
- The recommended deployment target is Linux x86_64 + Docker; Windows, macOS, ARM and very large Vaults are not fully supported yet — try them on a small scope first.
- Agent support centres on Dsh; other ACP agents need their own install/config. Layout analysis relies on self-hosted MinerU and the browser ONNX fallback is not fully validated on the web host.
- RAG: one Vault ↔ one knowledge base; image-based retrieval and PDF-original upload are out of scope; WeKnora rerank is off by default (slow on some instances).
- MCP has no auth. The web build does not provide a system tray, auto-update, Zotero Connector, SSH remote Vault, LaTeX compilation or a site-proxy gallery.

## Docs

- Deployment & operations: [`docs/usage/web-self-host.md`](docs/usage/web-self-host.md)
- User guides (desktop and shared features): [`docs/usage/index.md`](docs/usage/index.md)
- MCP tools & protocol: [`docs/backend/mcp.md`](docs/backend/mcp.md) (advanced)
- RAG providers & API: [`docs/backend/rag.md`](docs/backend/rag.md) (advanced)

---

## License

MIT (see [LICENSE](LICENSE)). This project is a derivative of Agentero; upstream copyright remains with its authors, and this repository's changes are released under MIT as well.
