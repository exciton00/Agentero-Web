# 自部署 Web 版（headless）

Agentero 的 headless Web 宿主把同一个 Vault 与工作台界面跑在一台**常开的机器**上（NAS / VPS / 小主机），用浏览器访问。
单用户设计：**不内置登录**；如需远程访问，请自行负责 TLS/鉴权/反向代理（本项目不提供方案）。

> 状态：核心链路已跑通并在浏览器验证：Vault 树 / 论文库 / PDF 阅读 / NOTES 编辑 / 设置 / 文件端点 / 多标签 watcher / 服务端任务中心 / 云同步 / 本地 PDF 与标识符导入 / Agent 对话（ACP，需在服务器安装 CLI）。Docker 镜像与 compose 已实测。设计见 [docs/development/webui.md](../development/webui.md)。

## 快速开始（Docker Compose，一个文件）

仓库只提交 `docker-compose.yml.example` + `.env.example`；实际文件被 `.gitignore` 忽略。
没有单独的 prod/host-agent compose。

```bash
git clone https://github.com/exciton00/Agentero-Web.git agentero && cd agentero
cp docker-compose.yml.example docker-compose.yml
cp .env.example .env
# 编辑 .env：Vault/数据路径、端口、可选 MCP 等
mkdir -p vault data

docker compose pull
docker compose up -d
open http://127.0.0.1:8787
```

关键点：

- 镜像不含 Agent CLI；compose 把宿主机 Dsh 包和 `~/.dsh` 挂进容器，Web host 用镜像内 Node 22
  执行挂载的 `lib/bin.js`，启动时自动注册 Dsh catalog 行。系统 npm 安装默认挂载：
  `${AGENTERO_HOST_DSH_ROOT}:/opt/host-agent/@deepseek-ai:ro` 和
  `${AGENTERO_DSH_HOME}:/data/home/.dsh`；对应 `.env` 见 `.env.example`。
- `vault/` 与 `data/` 是本地运行数据，已加入 `.gitignore`；生产建议把
  `AGENTERO_VAULT_DIR` / `AGENTERO_DATA_DIR` 指向仓库外绝对路径。
- 想用源码构建镜像：`.env` 设 `AGENTERO_IMAGE=agentero-web:local`，执行
  `docker compose up -d --build`。GHCR 包默认可能是 private：设 public 或先 `docker login ghcr.io`。

## 镜像构建（GitHub Actions → GHCR）

`.github/workflows/docker-image.yml` 在 push `main` / `v*` tag / 手动 dispatch 时构建
`linux/amd64` 镜像并推送到 `ghcr.io/<repository-owner>/agentero-web`（本仓库为
`ghcr.io/exciton00/agentero-web`），标签包括 `latest`（默认分支）、分支名、semver tag 与
`sha-<short>`；使用 `GITHUB_TOKEN`（`packages: write`）登录与 GitHub Actions 缓存。

部署时 `.env` 的 `AGENTERO_IMAGE` 指向该镜像，`docker compose pull && docker compose up -d --no-build`
即可；本地构建/调试用 `docker build -t agentero-web:local .`，并在 `.env` 设
`AGENTERO_IMAGE=agentero-web:local`。

## 环境变量（`.env`）

Docker Compose 的用户配置统一放在 `.env`（由 `.env.example` 复制）；Compose 会自动读取。
`.env` 与实际 compose 文件都不进 git。

| 变量 | 默认 | 说明 |
|---|---|---|
| `AGENTERO_IMAGE` | `ghcr.io/exciton00/agentero-web:latest` | 镜像地址；本地构建改 `agentero-web:local` |
| `AGENTERO_VAULT_DIR` / `AGENTERO_DATA_DIR` | `./vault` / `./data` | 宿主机 Vault / XDG 数据目录 |
| `AGENTERO_VAULT_CONTAINER_PATH` | `/vault` | 容器内 Vault 路径；与宿主 Vault 绝对路径一致可让容器内外路径相同 |
| `AGENTERO_WEB_BIND` / `AGENTERO_WEB_PORT` | `127.0.0.1` / `8787` | 宿主机绑定与端口 |
| `AGENTERO_WEB_ADDR` | `0.0.0.0:8787` | 容器内监听地址 |
| `AGENTERO_WEB_VAULT` | `/vault` | 容器内默认 Vault 根（命令仍以显式 `vaultPath` 为准） |
| `AGENTERO_WEB_DIST` | `/app/dist`（源码部署 `./dist`） | 前端构建产物目录 |
| `AGENTERO_WEB_ALLOW_ROOTS` | 空 | 额外允许文件端点读写的根目录，`:` 或 `,` 分隔 |
| `AGENTERO_WEB_MAX_BODY_MB` | `64` | `/api` 请求体上限（MiB，8–512，非法值回落 64） |
| `AGENTERO_MCP_ENABLED` | 空 | `1`/`true` 强制启动 MCP listener（无视 `settings.mcpEnabled`） |
| `AGENTERO_MCP_BIND` | 空 | MCP 监听地址 `host:port`，如 `0.0.0.0:8765` |
| `AGENTERO_MCP_ALLOWED_HOSTS` | 空 | MCP 额外允许的 `Host` 头（逗号分隔），反代域名必须列在这里 |
| `AGENTERO_DSH_HOME` | `${HOME}/.dsh` | 宿主机 Dsh 凭据/profile 目录，挂载到 `/data/home/.dsh` |
| `DEEPSEEK_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | 空 | 可选，服务端 Agent CLI 凭据 |
| `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_CACHE_HOME` | 系统默认 | 设置、使用记录、模型缓存；容器里已由 `/data` 挂载持久化 |
| `RUST_LOG` | `info` | 日志级别 |

完整模板见仓库根目录 [`.env.example`](../../.env.example)。

## MCP 客户端接入

设置 → 通用 → **MCP server** 打开后，服务端在独立端口（默认 `127.0.0.1:8765`）提供 Streamable HTTP MCP，端点 `http://127.0.0.1:8765/mcp`；开关与端口写回共享 `settings.json`，重启后保持。工具清单 / resources / 安全边界见 [后端 MCP 文档](../backend/mcp.md)。

- 本机客户端（Codex、Inspector、Claude Desktop 等）直接连 `http://127.0.0.1:8765/mcp`。
- 容器 / 远程：`AGENTERO_MCP_BIND=0.0.0.0:8765` 发布端口，`AGENTERO_MCP_ALLOWED_HOSTS=<外部域名>` 放行 `Host` 头，并由反向代理加鉴权（MCP 本身无鉴权，不要裸暴露公网）。
- 设置页开启 MCP 后即可用 MCP Inspector / Codex / 任何 Streamable HTTP 客户端连接 `http://<host>:<port>/mcp`；ChatGPT 等需要公网可达的客户端请走反向代理（web v1 不提供 Secure MCP Tunnel）。
- compose 示例（`docker-compose.yml` 注释）：`AGENTERO_MCP_ENABLED=1` + `AGENTERO_MCP_BIND=0.0.0.0:8765` + 发布 `8765`，并把外部域名加入 `AGENTERO_MCP_ALLOWED_HOSTS`。
- Web v1 不含 ChatGPT Secure MCP Tunnel；需要外网时用反代暴露 `/mcp`。
- `paper_text_get` 默认关闭；设置 → 通用 → 「对外暴露论文正文」打开后才返回 PDF 文本（opt-in，正文会发给 MCP 客户端）。

## 网络流量与客户端缓存

浏览器重复打开同一篇论文时不会反复下载整份 PDF：

- `GET /api/file` 返回 `ETag`（mtime+size）与 `Last-Modified`，带 `If-None-Match` / `If-Modified-Since` 的条件请求得到 **304**（无 body）；编辑器写入后 mtime/size 变化，缓存自然失效。
- `Cache-Control`：PDF / 图片 / 视频等媒体 `private, max-age=3600`（一小时内直接复用）；`layout.json`、`agentero-cite.json`、Markdown、TeX 等可编辑 sidecar 为 `private, no-cache`（每次必须 revalidate，避免读到旧内容）。
- `GET /api/file` 支持单区间 `Range` → **206**（PDF 阅读器跳页只取所需字节），不可满足的区间返回 416。
- 响应压缩：API JSON、Markdown、HTML、JS/CSS 等文本走 **gzip / brotli**（由 `Accept-Encoding` 协商）；PDF / 图片 / 视频等已压缩二进制不重复压缩，`/assets/*` 带 hash 的构建产物为 `public, max-age=31536000, immutable`。

反向代理若再做一层 gzip，建议直接放行上游的 `Content-Encoding`（或关掉代理压缩），避免二次压缩浪费 CPU。


## 数据布局

| 内容 | 位置 |
|---|---|
| 文献与笔记 | `AGENTERO_WEB_VAULT`（标准 Markdown/PDF + `.agentero/catalog.sqlite`） |
| 设置 | `$XDG_CONFIG_HOME/agentero/settings.json` |
| 使用记录 / 订阅 | `$XDG_DATA_HOME/agentero/*.sqlite` |
| 模型 / 缓存 | `$XDG_CACHE_HOME/agentero/` |

备份 = 拷贝 Vault 目录 + `data/`（XDG）目录；Vault 本身可被任何外部编辑器直接读取。

## 裸机运行（systemd）

```bash
cargo build --release -p agentero-web
pnpm build:web
AGENTERO_WEB_ADDR=127.0.0.1:8787 \
AGENTERO_WEB_VAULT=/srv/agentero/vault \
AGENTERO_WEB_DIST=$PWD/dist \
  ./target/release/agentero-web
```

`systemd` 单元里把这些环境变量写进 `Environment=` 即可；`PDFIUM_LIB_PATH` 指向 PDF 解析所需的 `libpdfium.so` 目录（镜像已内置）。

## 版面分析（自建 MinerU）

设置 → Layout → 选择 **MinerU**、打开 **Self-hosted**、填 Base URL 与 API Key（BYOK，只写入服务器 `settings.json`，不进镜像/仓库）。之后新入库或手动 Force re-parse 的论文由**服务端**调用 MinerU 解析，写 `source/layout.json` + `layout-index.json`；PDF 工具栏「Analysis」里的 FIGURES 面板展示检测到的图/表/算法/公式（缩略图 + caption + 页码跳转）。未配置远程服务时回退到浏览器本地 ONNX 模型。

**正文与图片**：`layout.json` 只描述版面；`PAPER.md` + `images/` 由 **Body parse engine** 生成。Web host 现已注册 MinerU / Paddle / OpenAI-compatible 三种云端正文解析引擎（凭据与 Analysis engine 共用同一 settings 配置池）。如果 Body parse engine 仍是 local，把 Analysis engine 切到 MinerU/Paddle 时会自动跟随；版面分析完成后若还没有 `PAPER.md`，会自动排队一次 ParseBody。之前 Web 只注册了本地 liteparse body parser，所以云端解析不会生成 `PAPER.md`/`images`。

## RAG 外部知识库（可选，仅 Web host；provider 抽象）

设置 → RAG：选择 **Provider**（当前可选 WeKnora；RAGFlow 预留、coming soon），打开开关、填 Base URL（可带/不带 `/api/v1`）、API Key、选择知识库（`rag_list_kbs` 下拉），可先「测试连接」。开启后服务端把当前 Vault 的每篇论文渲染成一份合并 Markdown 同步到该知识库（正文 + 图表索引 + NOTES + 批注文本；**PDF 与图片二进制不上传**），检索结果带论文路径/BibTeX `\cite{}`。

- **隐私**：显式 opt-in；笔记、批注、图注文本会发送到你配置的外部知识库（当前 provider=WeKnora）。API Key 只保存在服务端 `settings.json`（0600），前端只见掩码，重启后状态保持；旧 `settings.weknora` 会自动迁移为 `settings.rag`。
- **同步方式**：设置页「立即同步」（可取消、可看进度/失败项）或自动同步（默认关；开启后 Vault 写入 **60s 防抖** + 每 **30 分钟**兜底扫描）。内容按哈希增量更新，删除论文后下次同步清理对应 managed 文档；`rag_reconcile` 可从 KB `custom_metadata` 重建映射，「清理 Agentero 文档」需二次确认。
- **状态**：同步映射存 `$XDG_DATA_HOME/agentero/rag/<vault-key>.<provider>.json`（v2.0 旧路径 `<vault>.json` 会自动迁移；不在 Vault、不进 git、无密钥）；设置页显示已同步/待同步/失败计数与 `rag:sync` 进度。
- **单 Vault / 单知识库**（v1）：更换知识库会重置映射；同一 KB 多部署同时写属于未支持场景。出站请求遵循设置里的网络代理。
- **给外部 MCP / Web Agent 用**：设置里 `tools.exposeToMcp` 打开后，Agentero 的 MCP server（`/mcp`）会动态出现只读 `agentero_rag_search` / `agentero_rag_paper_context` / `agentero_rag_sync_status`；`tools.exposeToAgent` 打开后，Web 端 ACP agent（Chat）每轮会注入会话级 `agentero-rag` stdio MCP，可直接问"我库里有没有讨论 trapping/TCS 的笔记或图注，引用怎么写"。两者都只读、命中带 `paper.path` 与 `\cite{key}`，不会写入知识库；关闭开关后工具立即消失，不影响其余 MCP 工具。
- **限制**：图片语义检索、PDF 原件上传、provider 侧聊天/Agent 调用不在 v1；检索默认关闭 rerank（该实例开启时可能 20–40s 不返回）。单篇检索受 API key scope 限制（不支持 `knowledge_ids`）走 KB-wide 检索 + 本地过滤。provider 适配指南、API 细节与 RPC/事件契约见 [backend/rag.md](../backend/rag.md)，MCP 工具 schema 见 [backend/mcp.md](../backend/mcp.md)。

## Vault 同步（NAS 友好）

设置 → Sync 支持 **git 远程**（本地路径或 SSH URL）、S3 兼容存储、WebDAV：
- **git**：`gitRemote` 指向 NAS 上的 bare repo（如 `/srv/git/agentero-vault.git`）或 `ssh://nas/...`，凭据走系统 credential helper / SSH agent；无需对象存储即可多设备同步。
- S3/WebDAV 凭据同样只存在服务器 XDG `sync.json`。

## 按标签浏览文献

服务端自动维护 `<vault>/.agentero/tag-index.json`（隐藏缓存，可随时删除重建）。Library 表格上方固定一行标签筛选：左侧是已选标签 chips 与「清除标签筛选」，最右侧「标签筛选」下拉可搜索、多选（色点+计数）；多选按 **AND** 过滤，未选标签时显示全部论文，`#` 开头的 Host 保留标签（如 `#submitted:`）不进入筛选列表。与目录作用域叠加：选中 `papers/` 或子目录时同一行显示「当前目录: …」，列表递归过滤后再按标签 AND 过滤。

## BibTeX / BibLaTeX 网络挂载

设置 → 实验功能 → 「BibTeX / BibLaTeX 文献网络挂载」：总开关打开后，通过 `http(s)://<host>/agentero_online.bib` 实时提供当前 vault 的文献列表（内容格式由挂载类型 bibtex/biblatex 决定）；挂载范围可用标签（OR）与目录（`papers` 或其子目录）组合过滤。序列化在服务端本地完成，不会把元数据发给第三方 translation-server。

## 云同步自动运行

设置 → 同步 配置后开启「自动同步」，服务端会按所选间隔在后台执行（`sync_get_status` 返回 `autoSyncNextRunAt`/`lastOutcome`/`lastError`，刷新页面不丢）；手动「立即同步」与自动任务互斥。

## 大文件与流量

- RPC 请求体上限默认 64MB（`AGENTERO_WEB_MAX_BODY_MB=8..512` 可调）；PDF 直接走文件接口，不经过 RPC。
- 设置 → 版面解析 → 「版面解析页数上限」：超过该页数的 PDF 自动跳过版面解析（0 = 不限制），避免大文件上传/解析；后端也会兜底拒绝超限请求。
- 客户端缓存：`/api/file` 支持 ETag/304 与单区间 Range；PDF/图片 `private, max-age=3600`，JSON/文本 sidecar `no-cache`；静态 hashed 资源 `immutable`。重复打开 PDF 通常只需一次 304 校验，不重复下载。

## 接入 Agent（实测 Dsh）

Docker 镜像已包含测试过的 `@deepseek-ai/dsh@0.2.1-rc.2`，不需要在容器里安装 CLI，也不需要
宿主机 Dsh 二进制。只需要挂载宿主机的 `~/.dsh`（凭据 + profile）：

- compose 默认挂载 `${HOME}/.dsh`（可用 `.env` 的 `AGENTERO_DSH_HOME` 覆盖）；
- Web host 启动时检测镜像内 `dsh` + `/data/home/.dsh`，自动注册为正常 Dsh catalog 行；
- 源码/裸机运行 Web host 时，`dsh` 在 PATH 且 `~/.dsh` 存在也会自动注册。

`agentero-context` MCP 由 web host 自己 re-exec 提供，带论文/选区的对话可正常使用。

**Vault 路径对齐**：容器内默认 `/vault`。若要求宿主路径一致，在 `.env` 同时设：

```dotenv
AGENTERO_VAULT_DIR=/srv/agentero/vault
AGENTERO_VAULT_CONTAINER_PATH=/srv/agentero/vault
```

**凭据与自定义 Agent**：

- Dsh 凭据来自挂载的 `/data/home/.dsh`；其他 CLI 可用各自环境变量/配置文件。
- 其他 Agent 可 Settings → Agent 扫描/添加自定义 Agent（`agent_scan_catalog` / `agent_upsert_agent`）。
- 已验证：Dsh ACP、Agentero context MCP、Docker 内自动注册/探测/对话。
## 已知边界（当前）

- 单用户、无内置鉴权；不要直接把 8787 暴露到公网。
- Agent（ACP）需宿主机安装 Dsh 并挂载进容器；其他 ACP Agent 需自行安装/配置，未测试。
- Docker 使用宿主机的 Dsh 包 + `~/.dsh`（Linux）；自定义安装路径通过 `AGENTERO_HOST_DSH_ROOT` / `AGENTERO_DSH_HOME` 指定。
- 版面分析（ONNX）、Zotero Connector、远程 Vault（SSH）、自动更新在 Web 版不可用或不移植。
