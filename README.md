<p align="center">
  <img src="docs/assets/hero.png" alt="Agentero-Web" width="100%" />
</p>

<h2 align="center">Agentero-Web</h2>

<p align="center">
  可自部署的 Agentero Web 分支：在 NAS / VPS / 小主机上用浏览器打开完整科研工作台。
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <img src="https://img.shields.io/badge/AI--generated-code-8b5cf6" alt="AI-generated code" />
  <img src="https://img.shields.io/badge/maintenance-best--effort-lightgrey" alt="Maintenance: best effort" />
</p>

<p align="center">
  <a href="README.en.md">English</a> | <b>中文</b>
</p>

> **这是一个 fork，不是 Agentero 官方仓库。** 本仓库继承自 Agentero 的 `main` 分支 commit
> `be2d49ed00a1d7665f07af54a38f9602395f827d`（2026-10-21），在其之上做了 Web 化、MCP、RAG
> 等适配。上游桌面版文档与完整介绍请见 [Original Agentero](https://github.com/poco-ai/agentero)。

---

## 1. 继承关系（Provenance）

| 项目 | 值 |
|---|---|
| fork 来源 | `exciton00/Agentero`（Agentero 的 fork） |
| 原始上游 | [`poco-ai/agentero`](https://github.com/poco-ai/agentero) |
| **继承基线 commit** | **`be2d49ed00a1d7665f07af54a38f9602395f827d`**（Agentero `main`，2026-10-21，PR #808） |
| 本仓库基线 tag | `agentero-base`（指向上述 commit） |
| 本仓库改动 | 基线之后约 40+ 个 Web 相关提交（`git log agentero-base..main`） |
| 本仓库 HEAD | 见 `git log -1` |

继承方式：保留 Agentero 的完整 git 历史与桌面版代码；Web 分支主要新增 `crates/agentero-web`、
`crates/agentero-host`，把设置/搜索等能力迁入 `agentero-core`，并修改前端以支持无 Tauri 的
HTTP/WebSocket 宿主。桌面 `src-tauri` 除少量 re-export/共享设置外基本未改。

### 与上游同步

```bash
git remote add upstream git@github.com:exciton00/Agentero.git   # 若尚未添加
git fetch upstream
git merge upstream/main   # 或 rebase；Web 改动较多，冲突需自行解决
```

---

## 2. 功能清单（相对继承基线新增/适配）

> 详细分轮说明见 [`docs/development/webui.md`](docs/development/webui.md)，部署运维见
> [`docs/usage/web-self-host.md`](docs/usage/web-self-host.md)。

### 2.1 Headless Web 宿主

- 新增 `crates/agentero-web`：axum HTTP/WebSocket 服务端，浏览器直接使用现有 React SPA。
- `POST /api/rpc/<command>` 兼容桌面 Host command（RPC 100+ 条：settings / vault / paper /
  trash / wiki / search / marks / import / jobs / sync / agent / doctor 等）。
- `GET /api/events` WebSocket 事件总线，多标签页独立窗口 label 与定向事件路由。
- `/api/file`、`/api/upload`、`/api/dir|mkdir|rename` 文件端点；Vault watcher；服务端任务中心；
  `/api/healthz` 健康检查；SPA 静态托管。
- 前端新增浏览器 transport 垫片（`@tauri-apps/*` → web transport），新增
  `pnpm build:web`；`isTauri()` 分诊逐步改为 `hasHostTransport()`；服务端 Vault 在 web 启动时权威生效。
- 服务端 PDF 导入 / 标识符导入 / BibTeX 导入 / `PAPER.md` 正文解析 / 引用解析；PDFium worker
  提前到 tokio 启动前 dispatch。
- 单用户设计：**无内置鉴权**，请放在 TLS + 鉴权的反向代理之后。

### 2.2 Agent（ACP，服务端运行）

- 新增 tauri-free 的 `crates/agentero-host`（约 20K LOC）：ACP agent 域从桌面移植，
  Host/HostSink 接缝抽象；覆盖 agent 探测/目录/配置、`agent_run_once`、流式
  `agent:status/stream/tool/usage/completed`、会话 history/load/resume、warm pool、权限/elicitation 门。
- 真实 Agent 已在浏览器端到端验证：服务器安装 CLI（本仓库实测 Dsh）→ 设置页注册 →
  Chat 面板中文提问 → 真实模型流式返回（含 reasoning/工具事件与论文上下文）。
- 每个标签页的 agent 事件只投递到发起标签；支持后台 workflow（paper-reader 等）。

### 2.3 文献库 / PDF / 笔记

- 沿用 Agentero 的 Vault / catalog / PDF 阅读 / NOTES 编辑 / 双链 / marks。
- Web 适配：论文库按标签筛选（服务端 `tag-index.json` 隐藏缓存、AND 语义、目录 scope 叠加）；
  单击选中 / 双击打开；多选与批量操作；PDF/版面解析状态图标；覆盖上传 PDF；批量删除。
- 论文右键：添加标签、删除 PDF、删除版面解析文件、覆盖解析等；Library 单元格长按复制
  （替代会与双击打开冲突的双击复制）；Vault 菜单隐藏桌面专属项。
- 服务器端导入：本地 PDF、arXiv/DOI 标识符、BibTeX；渲染任务 offer/report 协议（浏览器执行）；
  `AGENTERO_WEB_IMPORT_SERVER_FALLBACK=1` 可在无浏览器/CI 场景走服务端导入。

### 2.4 版面解析（自建 MinerU）

- 设置 → Layout：选择 MinerU（Self-hosted）、填 Base URL / API Key（BYOK，仅运行时 settings）；
  服务端调用远程 MinerU 解析，写入 `source/layout.json` + `layout-index.json`。
- **版面分析引擎与正文解析引擎是两项设置**：Analysis engine 产出 layout sidecar；Body parse engine
  产出 `PAPER.md` + `images/`。Web 现已注册 MinerU / Paddle / OpenAI-compatible 三种云端正文解析引擎
  （之前只有本地 liteparse，云端 body parser 没有注册，所以不会出现 PAPER.md/images）。
- 从本地默认（local）把 Analysis engine 切到 MinerU/Paddle 时，Body parse engine 也会自动跟随；
  版面分析完成后若还没有 `PAPER.md`，会再自动排队一次 ParseBody。
- PDF 工具栏 Analysis → FIGURES 面板（缩略图 + caption + 页码）；`layout_index` 供 MCP/CLI 使用。
- 版面解析页数上限 `layout.maxPages`（0 = 不限）：前端超限跳过、后端兜底拒绝；`/api` body
  limit 默认 64MB（`AGENTERO_WEB_MAX_BODY_MB` 可调），解决大 PDF base64 超限。

### 2.5 MCP Server（Web host 内嵌）

- 桌面版 Streamable HTTP MCP（24 tools + resources）移植到 web：独立 listener，默认
  `127.0.0.1:8765/mcp`；设置页开关、端口热改、状态事件。
- 环境变量：`AGENTERO_MCP_ENABLED` / `AGENTERO_MCP_BIND` / `AGENTERO_MCP_ALLOWED_HOSTS`；
  容器/反代场景可强制开启并放行 Host；非 loopback 会打无鉴权 WARN。
- 工具含 paper/notes/tags/vault_search/file/layout/page_read 等；`paper_text_get` 仍为
  opt-in（默认关闭）；ChatGPT Secure MCP Tunnel 不在 Web v1 范围。

### 2.6 RAG（外部知识库，provider 抽象）

- 设置 → RAG：把 Vault 每篇论文渲染为一份合并 Markdown 同步到外部知识库；**PDF 与图片
  二进制不上传**，内容含正文、图表索引、NOTES、划词/visual 批注文本与 BibTeX 引用索引。
- provider 抽象：`RagProviderId` / `RagCapabilities` / `RagBackend`；当前实现 **WeKnora**，
  RAGFlow 槽位预留（未实现）。
- 每篇文档带 `BibTeX key`、`\cite{}` 与 bibtex + biblatex 两段条目，key 与
  `/agentero_online.bib` 完全一致；检索结果带 `paper.path/title/tags/bibtexKey/latex`。
- 同步：手工 Markdown 原地更新、内容哈希增量、managed 文档才删除；映射存 XDG data
  （不入 Vault/git）；自动同步可选（写入后 60s 防抖 + 30 分钟兜底扫描）。
- 工具：MCP 动态新增 **`agentero_rag_search` / `agentero_rag_paper_context` /
  `agentero_rag_sync_status`**（总计 27 个工具；关闭后回到 24）；web agent（ACP）会话自动注入
  只读 `agentero-rag` stdio MCP，可直接问“库里有没有讨论 X 的笔记/图注、引用怎么写”。
- Provider 扩展：新增 RAGFlow 等只需实现 `RagBackend`/capabilities，详见
  [`docs/backend/rag.md`](docs/backend/rag.md)。

### 2.7 同步、BibTeX 挂载与稳定性

- 云同步（git/S3/WebDAV）在 headless host 完整可用；服务端自动同步调度器。
- `GET /agentero_online.bib`：按设置（bibtex/biblatex、tag/dir 过滤）实时输出当前 Vault 的
  bibliography；本地序列化，不经第三方 translation-server。
- 自动刷新根因修复：watcher 批量合并 + WriteBurst 延迟、前端跨帧合并 + 共享刷新协调器
  （一次导入 96 帧事件 → 2–3 帧、刷新 3 波 → 2 波）。
- HTTP 缓存/性能：ETag/Last-Modified/304、单区间 Range、媒体 `private,max-age=3600`、
  sidecar `no-cache`、gzip/br、hashed 静态资源 immutable；首屏约 252KB（br）。
- MCP listener 端口热改/并发串行化修复；侧边栏隐藏/显示后拖拽宽度修复；设置返回工作台修复。

### 2.8 部署交付

- `Dockerfile` 三阶段构建（Node 22 + PDFium + tesseract 预置，镜像内 `agentero-web` +
  `dist` + PDFium），非 root runtime、healthcheck。
- `docker-compose.yml`（普通与 `prod` profile 共用）+ `deploy/Caddyfile`
  （Caddy TLS + basic auth、不直接发布应用端口）。
- 源码部署支持（Rust + pnpm）；环境变量、数据目录、备份与 MCP/RAG 配置说明见文档。

---

## 3. AI 生成声明

**本仓库绝大部分代码、测试、文档与提交信息由 AI（DeepSeek Harness 的 coding agents）生成，
经有限的人工审查与运行时验证。** 它**不保证**正确性、安全性、完整性或适用于生产环境；
请在使用前自行审计代码、限制网络/文件权限，并对数据做好备份。继承自 Agentero 的代码版权与
许可仍归原作者（见 [LICENSE](LICENSE)）。

---

## 4. 维护声明

- 本仓库**不承诺持续维护**：不保证 issue/PR 会被响应，不提供 SLA、版本节奏或安全修复承诺。
- 上游 Agentero 更新不保证合并；桌面版/上游文档可能与本 fork 不一致。
- 你可以自由 fork；修复与改进建议自行保留或向上游回馈。

---

## 5. 部署

### 5.1 Docker Compose（普通与生产用同一个文件）

前置：Linux/macOS + Docker Compose 插件（Windows 走 Docker Desktop 也可）。

仓库只提交两个模板：`docker-compose.yml.example` 和 `.env.example`；复制出来的实际文件被
`.gitignore` 忽略。**只有一个 compose 文件，没有单独的 prod/host-agent 文件。**

```bash
git clone https://github.com/exciton00/Agentero-Web.git agentero-web
cd agentero-web

cp docker-compose.yml.example docker-compose.yml
cp .env.example .env
# 编辑 .env：Vault/数据路径、端口、可选 MCP 等
mkdir -p vault data

# 拉取 GHCR 镜像并启动（镜像包含前端、PDFium、Node 22 和测试过的 Dsh）
docker compose pull
docker compose up -d

# 打开 http://127.0.0.1:8787
```

**Dsh Agent**：镜像已内置测试过的 `@deepseek-ai/dsh@0.2.1-rc.2`；compose 默认把宿主机
`~/.dsh`（凭据 + profile）挂到 `/data/home/.dsh`，Web host 启动时自动注册为正常的
`Dsh` catalog 行。宿主机只需要有一个已登录的 `~/.dsh`，不需要在容器里装 Dsh，也不需要
host-agent 覆盖文件。Windows/自定义 Dsh 目录可在 `.env` 设置：

```dotenv
AGENTERO_DSH_HOME=C:/Users/you/.dsh
```

源码/裸机运行 Web host 时同理：`dsh` 在 PATH 且 `~/.dsh` 存在就会自动注册。

**生产（同一 compose 文件）**：生产专属的 Caddy 服务放在 `prod` profile 里，默认不启动：

```bash
# 在 .env 设置：
# AGENTERO_DOMAIN=agentero.example.com
# AGENTERO_BASIC_AUTH_USER=you
# AGENTERO_BASIC_AUTH_HASH='...'   # docker run --rm caddy:2 caddy hash-password --plaintext '...'
docker compose --profile prod pull
docker compose --profile prod up -d
```

Caddy 会接管 80/443，应用端口仍只绑 loopback。`deploy/Caddyfile` 无需手改，域名和 basic auth
由 `.env` 注入；反向代理必须支持 WebSocket（`/api/events` Upgrade）。

**Vault 路径对齐**：容器内默认 `/vault`。如果宿主工具需要和宿主机完全一致的绝对路径，在
`.env` 同时设：

```dotenv
AGENTERO_VAULT_DIR=/srv/agentero/vault
AGENTERO_VAULT_CONTAINER_PATH=/srv/agentero/vault
```

**关键点**：
- `AGENTERO_VAULT_DIR` 是 Vault（Markdown/PDF/`.agentero/catalog.sqlite`）；`AGENTERO_DATA_DIR`
  是 XDG 设置/使用记录/同步状态/RAG state，均需备份。
- 镜像地址由 `.env` 的 `AGENTERO_IMAGE` 控制；fork 后改成
  `ghcr.io/<你的账号>/agentero-web:latest`。本地源码构建可改 `AGENTERO_IMAGE=agentero-web:local`
  并执行 `docker compose up -d --build`。GHCR 包默认可能是 private：公开前在 Packages 里设为 public
  或先 `docker login ghcr.io`。
- 需要公网 MCP 时：`.env` 设 `AGENTERO_MCP_ENABLED=1`、`AGENTERO_MCP_BIND=0.0.0.0:8765`，
  取消 compose 里 MCP 端口映射注释并发布 8765；把外部域名加入 `AGENTERO_MCP_ALLOWED_HOSTS`，
  并由反代加鉴权（MCP 本身无鉴权）。

### 5.2 源码部署

前置：Rust stable、Node 22+、pnpm 11、Linux。PDF 解析需要 `libpdfium`（可执行
`pnpm pdfium:stage` 下载到 `src-tauri/pdfium/`，运行时用 `PDFIUM_LIB_PATH` 指向该目录）；
可选 tesseract。

```bash
git clone https://github.com/exciton00/Agentero-Web.git agentero-web
cd agentero-web

pnpm install
pnpm build:web

# 可选：为 ACP agent CLI 准备打包适配器（仅使用内置 adapter 时需要）
pnpm adapters:stage

cargo build --release -p agentero-web

mkdir -p /srv/agentero/{vault,data,config,cache}
AGENTERO_WEB_ADDR=127.0.0.1:8787 \
AGENTERO_WEB_VAULT=/srv/agentero/vault \
AGENTERO_WEB_DIST=$PWD/dist \
XDG_CONFIG_HOME=/srv/agentero/config \
XDG_DATA_HOME=/srv/agentero/data \
XDG_CACHE_HOME=/srv/agentero/cache \
PDFIUM_LIB_PATH=$PWD/src-tauri/pdfium \
RUST_LOG=info \
  ./target/release/agentero-web
```

用 systemd 托管时把上述环境变量写入 `Environment=`；反向代理配置见
[`docs/usage/web-self-host.md`](docs/usage/web-self-host.md)。首次访问进入欢迎页，新建/打开 Vault 即可。

### 5.3 环境变量（`.env`）

Docker Compose 的所有用户配置统一放在 `.env`（从 `.env.example` 复制）。
`docker compose` 会自动读取仓库根目录的 `.env`；这个文件与实际的 compose 文件都被 git 忽略。

| 变量 | 默认 | 说明 |
|---|---|---|
| `AGENTERO_IMAGE` | `ghcr.io/exciton00/agentero-web:latest` | 镜像地址；本地构建可改 `agentero-web:local` |
| `AGENTERO_VAULT_DIR` / `AGENTERO_DATA_DIR` | `./vault` / `./data` | 宿主机 Vault / XDG 数据目录 |
| `AGENTERO_VAULT_CONTAINER_PATH` | `/vault` | 容器内 Vault 路径；与 `AGENTERO_VAULT_DIR` 设为同一绝对路径可实现宿主机/容器路径一致 |
| `AGENTERO_WEB_BIND` / `AGENTERO_WEB_PORT` | `127.0.0.1` / `8787` | 宿主机绑定与端口 |
| `AGENTERO_WEB_ADDR` / `AGENTERO_WEB_DIST` | 容器内固定路径 | 容器内 Web 监听 / 前端 dist |
| `AGENTERO_WEB_MAX_BODY_MB` | `64` | `/api` body 上限（8–512 MiB） |
| `AGENTERO_WEB_ALLOW_ROOTS` | 空 | 文件端点额外根目录（`:`/`,` 分隔） |
| `AGENTERO_MCP_ENABLED` / `AGENTERO_MCP_BIND` / `AGENTERO_MCP_ALLOWED_HOSTS` | 空 | MCP 强制开启 / 绑定地址 / Host 白名单 |
| `AGENTERO_DOMAIN` / `AGENTERO_BASIC_AUTH_USER` / `AGENTERO_BASIC_AUTH_HASH` | 仅生产 | Caddy 域名与 basic auth（hash 用单引号，防止 `$` 插值） |
| `AGENTERO_DSH_HOME` | `${HOME}/.dsh` | 宿主机 Dsh 凭据/profile 目录，挂载到 `/data/home/.dsh` |
| `DEEPSEEK_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | 空 | 可选，服务端 Agent CLI 凭据 |
| `RUST_LOG` | `info` | 日志级别 |

完整模板见 [`.env.example`](.env.example)。

### 5.4 镜像构建（GitHub Actions → GHCR）

仓库内置 [`.github/workflows/docker-image.yml`](.github/workflows/docker-image.yml)：

- 触发：推送 `main`、推送 `v*` tag、手动 `workflow_dispatch`；
- 构建 `linux/amd64` 镜像并推送到 **`ghcr.io/<repository-owner>/agentero-web`**
  （当前仓库为 `ghcr.io/exciton00/agentero-web`）：`latest`（默认分支）、分支名、semver tag、
  `sha-<short>`；
- 使用 `GITHUB_TOKEN` 登录 GHCR，`packages: write`；构建缓存走 GitHub Actions cache
  （`type=gha`）；镜像内已包含前端 `dist`、PDFium 与 Node 22；
- 构建成功后，把 `.env` 的 `AGENTERO_IMAGE` 指向 `ghcr.io/exciton00/agentero-web:latest`，
  `docker compose pull && docker compose up -d --no-build` 即可部署；
- 本地构建/调试：`docker build -t agentero-web:local .`，或在 `.env` 设
  `AGENTERO_IMAGE=agentero-web:local` 后 `docker compose up -d --build`。

> GHCR 包默认可能是 private；公开部署前可在 GitHub 仓库 → Packages 里把包设为 public，
> 或先 `docker login ghcr.io -u <user>`（PAT 需要 `read:packages`）。

---

## 6. 实测环境与未测试事项（请务必阅读）

本 fork 的功能只在以下环境实际跑过；**其他组合未测试、不保证兼容**：

| 类别 | ✅ 已实测 | ❌ 未测试 / 不保证 |
|---|---|---|
| **Agent** | **Dsh**（`dsh --profile acp`，0.2.1-rc.2）+ 真实模型对话、工具/流式/历史、paper-reader 精读 | Claude Code、Codex、OpenCode 及其他 ACP agent；其他模型/API key/provider |
| **版面解析** | **自建 MinerU**（remote API，`layout_remote_probe` 显示 `mineru 4.0.9`；9 页/15 页真实 PDF 解析） | 官方 MinerU 云（mineru.net）、其他 MinerU 版本；PaddleOCR / OpenDataLoader / MarkItDown / 其他版面引擎；Web 端本地 ONNX fallback |
| **RAG** | **WeKnora**（API 与 v0.8.3 文档对齐的实例；6 篇论文同步、tags、BibTeX 引用、`agentero_rag_*` MCP、provider 迁移） | RAGFlow（仅预留 provider 槽位，未实现）；其他 RAG 系统；其他 WeKnora 版本/旧版 API |
| **MCP 客户端** | 自带 Python Streamable HTTP 测试客户端（initialize/tools/list/tools/call） | ChatGPT、Claude Desktop、MCP Inspector、Codex 等第三方客户端；Secure MCP Tunnel（Web v1 不提供） |
| **OS / 容器** | Linux x86_64（Debian 13），Docker 29.7.2 / Docker Compose；开发机 Node 24（镜像 Node 22） | Windows、macOS、ARM、其他 Linux 发行版 |
| **浏览器** | Chromium（自动化测试）；桌面宽度视口 | Firefox、Safari、移动端浏览器、极端分辨率 |
| **反向代理** | 开发 compose 直连 8787；生产 Caddy 编排已提供 | Caddy 生产部署未做完整公网验收；nginx/Tailscale/Frp 等未测试 |
| **数据/规模** | 6 篇测试 Vault、单 Vault、单知识库 | 大规模 Vault（数百/数千篇）、多用户、多 Vault 并发、跨设备同时写 |

> 如果你启用未测试的 Agent/解析引擎/RAG，请先小范围试跑；错误/兼容性问题自行承担。

---

## 7. 配置 WeKnora RAG（API Key 权限说明）

RAG 当前唯一**已实现**的 provider 是 **WeKnora**；RAGFlow 在 UI 中显示为 coming soon。
以下步骤在 WeKnora 侧完成（测试基于与 v0.8.3 API 文档对齐的实例）：

### 7.1 创建知识库

1. 登录 WeKnora，创建/打开一个用于 Agentero-Web 的知识库（document 类型）。
2. 确保它已完成初始化（embedding / 检索配置可用；WeKnora 的问答模型不是 Agentero 必需，但
   索引/向量检索需要 KB 的 embedding 配置正常）。
3. 记下 **Knowledge Base ID**（KB 页面 URL 或 `GET /api/v1/knowledge-bases` 返回的 `id`）。

### 7.2 创建 API Key（能力与范围）

WeKnora 控制台：**空间设置 → API Key → 新建**（或 API `POST /api/v1/tenants/<tenant>/api-keys`）。
Agentero-Web 只需要以下两项能力：

| 能力 | 用途（Agentero-Web 实际调用） |
|---|---|
| **`retrieve`**（检索/读取） | 读取 KB 列表/详情、知识列表/详情、`knowledge-search`、读取标签 |
| **`ingest`**（入库/写入） | 创建/更新/删除手工 Markdown 知识、创建标签、批量给知识赋标签 |

**不需要** `manage_kbs`（Agentero-Web 不创建/删除知识库本身）、`chat`（不调用 WeKnora 问答）、
`manage_models`、`manage_members` 或 `FullAccess`。若 WeKnora 的 API Key 表单支持
**Knowledge Base allow-list / 可访问知识库**，请只勾选上面创建的这个 KB。

> 最小权限示例：`capabilities = ["retrieve", "ingest"]` + `knowledge_base_ids = ["<KB_ID>"]`。
> 使用 full-access 也可以工作，但不推荐；使用过宽 key 时请自行做网络侧限制。

### 7.3 填入 Agentero-Web

1. 打开 **Settings → RAG / External knowledge base**。
2. **Provider** 选择 `WeKnora`。
3. **Base URL**：`https://<your-weknora-host>/api/v1`（也可只填主域名，客户端会归一化）。
4. **API Key**：粘贴上一步创建的 key（仅保存在服务端 `settings.json`，前端只显示掩码）。
5. **Knowledge Base**：从下拉选择知识库（或填 KB ID）。
6. 点 **Test connection**（应显示 KB 名称/文档数）→ 打开 **Enable RAG** → 选择同步内容 →
   **Sync now**；同步完成后 KB 中会出现每篇论文一个 Markdown 文档（PDF/图片二进制不上传）。
7. 需要外部 MCP / web agent 使用 RAG 时，保持 `tools.exposeToMcp` / `tools.exposeToAgent` 打开；
   关闭会立即移除 `agentero_rag_*` 工具。

### 7.4 注意事项

- WeKnora 的 API Key 必须包含 **retrieve + ingest**；只有 retrieve 时 Test 可能通过，但同步会失败。
- API Key 只存在服务端 XDG settings（0600），日志/RPC/同步状态不含 key；请勿把 API Key 提交到
  git 或放进浏览器书签/前端环境变量。
- 若 WeKnora key scope 不支持按 `knowledge_ids` 限定，Agentero-Web 会退化为 KB 级检索 + 本地
  过滤（功能正常，结果一致）。
- RAGFlow/其他 provider 需要自行实现 `RagBackend` 并在 `RagProviderId` 注册；
  RAGFlow 当前未实现，选择后返回可读的 `not_implemented`。

---

## 8. 已知限制（当前）

- 单用户、**无内置鉴权**；不要把 8787 直接暴露公网；请用反代 + 鉴权。
- Agent（ACP）依赖服务器安装 CLI 与凭据；本 fork 只实测 Dsh。
- 版面解析依赖自建 MinerU；本地 ONNX fallback 在 Web 端未做端到端验证。
- RAG：单 Vault ↔ 单知识库；图片像素/PDF 原件不上传；RAGFlow 未实现；WeKnora 的 rerank 在
  测试实例上可能很慢（默认关闭，超时降级）。
- 不支持多用户/租户隔离、自动更新、系统托盘/原生菜单、Zotero Connector、SSH 远程 Vault、
  LaTeX 编译、站点代理广场（保留基础 PDF 下载）。
- MCP 无鉴权；公网 MCP 必须由反向代理保护，并使用 `AGENTERO_MCP_ALLOWED_HOSTS` 放行 Host。
- 桌面版（src-tauri）仍可编译，但本 fork 不再以桌面发布为主要目标。

---

## 9. 文档索引

- 设计 / 里程碑 / 分轮修复：[`docs/development/webui.md`](docs/development/webui.md)
- 部署与运维：[`docs/usage/web-self-host.md`](docs/usage/web-self-host.md)
- RAG provider 契约与 WeKnora 实现：[`docs/backend/rag.md`](docs/backend/rag.md)
- MCP 工具与构建：[`docs/backend/mcp.md`](docs/backend/mcp.md)
- 原 Agentero 桌面/CLI/数据模型文档：`docs/backend/*`（继承自上游，部分章节可能未覆盖 Web 差异）

---

## 10. License

MIT，继承自 Agentero（见 [LICENSE](LICENSE)）。原项目版权归其作者所有；本 fork 的修改也以 MIT
发布，不提供任何担保。
