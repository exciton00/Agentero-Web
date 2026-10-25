<p align="center">
  <img src="docs/assets/hero.png" alt="Agentero-Web" width="100%" />
</p>

<h2 align="center">Agentero-Web</h2>

<p align="center">
  可自部署的科研工作台：在 NAS / VPS / 小主机上运行，用浏览器打开完整的 Agentero 工作区。
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
</p>

<p align="center">
  <a href="README.en.md">English</a> | <b>中文</b>
</p>

---

## 这是什么

Agentero-Web 把 Agentero 的科研工作台搬到浏览器：论文、PDF、笔记和标注都存放在你自己的 **Vault**（普通文件夹）里，应用在一台常开的机器上运行，你在电脑、平板或手机的浏览器里访问同一个工作区。

- 单用户、自托管；数据与凭据都在你自己的机器上。
- **不内置登录或反向代理**：默认只监听 `127.0.0.1`，需要远程访问时由你自行加 TLS / 鉴权 / 反向代理。
- Vault 里的 Markdown、PDF、`marks/` 离开应用仍可用任何编辑器、终端或 Git 打开。

## 功能

- **浏览器工作台**：文件树、论文库、PDF 阅读、Markdown / NOTES 编辑、多面板工作区、多标签页窗口。
- **论文库与导入**：按目录与标签浏览筛选；支持 DOI / arXiv / BibTeX / 本地 PDF 导入，批量操作、覆盖 PDF、重新解析。
- **PDF 阅读与标注**：划词高亮、颜色切换、页边备注；右侧「批注」面板分「高亮与备注 / 对话 / 视觉批注」，支持跳转、展开备注与删除。标注保存在论文目录的 `marks/` 中，不改写原始 PDF。
- **翻译**：划词翻译与整篇版面翻译；**Settings → 翻译**里可调「正文字号」「标题字号」（百分比，实时生效）并一键恢复默认。
- **版面与正文解析**：接入自建 MinerU 等云端版面/正文解析服务，生成 `layout.json`、`PAPER.md` 与 `images/`；可设置解析页数上限。
- **Agent（ACP）**：使用宿主机安装的 Agent CLI（当前支持 Dsh），在 Chat 面板里带论文与选区上下文对话；每个标签页的事件独立路由。
- **MCP Server**：把 Vault 的论文/笔记/文件能力通过 Streamable HTTP MCP 提供给外部客户端；工具与论文正文默认按需开启。
- **外部知识库 RAG**：把论文渲染成 Markdown 同步到 WeKnora 等外部知识库，并把只读检索工具提供给 MCP 客户端与 Web Agent。
- **同步与引用**：Vault 云同步（git / S3 / WebDAV；当前版本不建议开启，见快速开始）；`/agentero_online.bib` 实时提供 BibTeX / BibLaTeX 文献列表。

## 快速开始（Docker Compose）

前置：Linux + Docker Compose 插件。

```bash
git clone https://github.com/exciton00/Agentero-Web.git agentero-web
cd agentero-web

cp docker-compose.yml.example docker-compose.yml
cp .env.example .env
# 编辑 .env：Vault / 数据路径、端口、宿主机 Dsh 路径、可选 MCP 等
mkdir -p vault data

docker compose pull
docker compose up -d
# 打开 http://127.0.0.1:8787
```

- 仓库只提供一份 `docker-compose.yml.example` + `.env.example`，复制出的实际文件不会被提交；部署不包含反向代理或生产编排。
- 默认使用已发布的 `ghcr.io/exciton00/agentero-web:latest` 镜像。需要自己构建时，把 `.env` 的 `AGENTERO_IMAGE` 改为本地 tag，执行 `docker compose up -d --build` 即可。
- 如果要从原版 Agentero 迁移已有数据：先退出原应用，再把原 Agentero 的 Vault 目录内容手动复制到 `AGENTERO_VAULT_DIR`；不要让两个应用同时写入同一个 Vault。
- 当前版本不建议开启云同步：Agentero 与 Agentero-Web 的同步状态/数据格式可能不兼容，混用可能产生冲突。

### 接入 Dsh Agent（宿主机挂载）

镜像**不包含** Agent CLI。要让 Chat 面板可用，请使用宿主机上已安装并登录的 Dsh：

```dotenv
# .env（以 root + system npm 安装为例）
AGENTERO_HOST_DSH_ROOT=/usr/lib/node_modules/@deepseek-ai
AGENTERO_DSH_HOME=/root/.dsh
```

compose 会把这两个路径分别挂载到容器的 `/opt/host-agent/@deepseek-ai`（只读）和 `/data/home/.dsh`，Web host 启动时自动注册为正常的 `Dsh` catalog 行。

- 若用 nvm / volta 安装：执行 `readlink -f "$(command -v dsh)"`，去掉结尾的 `/dsh/lib/bin.js`，把剩余目录填入 `AGENTERO_HOST_DSH_ROOT`；`~/.dsh` 的绝对路径填入 `AGENTERO_DSH_HOME`。
- 复用旧版本留下的 `./data` 卷后若看不到 Dsh，打开 **Settings → Agent →「检测宿主机 Dsh」→「重新检测」**，清除旧标记并重新注册即可。
- 其他 ACP Agent 需自行安装并完成登录；Web host 通过挂载/环境变量访问它们。

**Vault 路径对齐**：容器内 Vault 默认为 `/vault`。如果宿主机工具需要和容器里完全一致的绝对路径，在 `.env` 同时设置：

```dotenv
AGENTERO_VAULT_DIR=/srv/agentero/vault
AGENTERO_VAULT_CONTAINER_PATH=/srv/agentero/vault
```

### 常用环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `AGENTERO_IMAGE` | `ghcr.io/exciton00/agentero-web:latest` | 使用的镜像 |
| `AGENTERO_VAULT_DIR` / `AGENTERO_DATA_DIR` | `./vault` / `./data` | 宿主机 Vault / 应用数据（XDG）目录 |
| `AGENTERO_VAULT_CONTAINER_PATH` | `/vault` | 容器内 Vault 路径 |
| `AGENTERO_WEB_BIND` / `AGENTERO_WEB_PORT` | `127.0.0.1` / `8787` | 宿主机绑定地址与端口 |
| `AGENTERO_HOST_DSH_ROOT` | `/usr/lib/node_modules/@deepseek-ai` | 宿主机 Dsh 包根目录（挂载给容器使用） |
| `AGENTERO_DSH_HOME` | `${HOME}/.dsh` | 宿主机 Dsh 凭据 / profile 目录 |
| `AGENTERO_MCP_ENABLED` / `AGENTERO_MCP_BIND` / `AGENTERO_MCP_ALLOWED_HOSTS` | 空 | MCP 强制开启 / 监听地址 / `Host` 白名单 |
| `AGENTERO_WEB_MAX_BODY_MB` | `64` | `/api` 请求体上限（8–512 MiB） |
| `AGENTERO_WEB_ALLOW_ROOTS` | 空 | 文件接口额外允许的根目录 |
| `AGENTERO_SYNC_ACCESS_KEY` / `AGENTERO_SYNC_SECRET_KEY` | 空 | 可选，云同步凭据 |
| `DEEPSEEK_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | 空 | 可选，服务端 Agent CLI 凭据 |
| `RUST_LOG` | `info` | 日志级别 |

完整模板见 [`.env.example`](.env.example)。

## 数据与备份

| 内容 | 位置 |
|---|---|
| Vault（Markdown / PDF / `marks/` / `.agentero/catalog.sqlite`） | 宿主机 `AGENTERO_VAULT_DIR` |
| 设置、云同步 / RAG 状态、使用记录、缓存 | 宿主机 `AGENTERO_DATA_DIR`（容器内 XDG `/data`） |

备份 = 拷贝 Vault 目录 + 数据目录。Vault 本身可被任何外部编辑器直接读取；升级镜像前建议先备份。

## 常用设置

- **Settings → Agent**：检测宿主机 Dsh；Dsh 缺失或沿用旧数据卷时点「重新检测」。
- **Settings → 翻译**：选择翻译服务；全文翻译的正文字号 / 标题字号可调并恢复默认。
- **Settings → 版面解析**：选择版面分析与正文解析引擎（如自建 MinerU）、设置解析页数上限。
- **Settings → 同步**：git / S3 / WebDAV 云同步；当前版本不建议开启（见快速开始）。
- **Settings → 通用 → MCP server**：开启/关闭 MCP 监听端口。
- **Settings → RAG / External knowledge base**：配置外部知识库，见下文。

## MCP 客户端接入

在 **Settings → 通用 → MCP server** 打开开关后，服务端在独立端口提供 Streamable HTTP MCP，默认：

```
http://127.0.0.1:8765/mcp
```

- 本机客户端（MCP Inspector、Codex 等）直接连上述地址即可；应用内的 Agent 面板走 ACP，不经过 MCP。
- 需要给容器 / 远程客户端使用时：`.env` 设 `AGENTERO_MCP_ENABLED=1`、`AGENTERO_MCP_BIND=0.0.0.0:8765`，发布 8765 端口，并把外部域名加入 `AGENTERO_MCP_ALLOWED_HOSTS`。
- MCP **没有内置鉴权**；公网暴露必须由你的反向代理加鉴权，不要直接裸暴露端口。
- 论文正文默认不通过 MCP 返回；需要在 **Settings → 通用** 打开「对外暴露论文正文」后才会返回文本。
- 工具清单、协议与安全边界见 [MCP 工具文档](docs/backend/mcp.md)（进阶）。

## 外部知识库 RAG

把 Vault 中的论文同步到外部知识库，供 MCP 客户端与 Web Agent 做只读语义检索。当前支持 **WeKnora**（RAGFlow 预留，显示为 coming soon）。

1. 在 WeKnora 创建并初始化一个知识库（document 类型），记下 **Knowledge Base ID**。
2. 创建 API Key，授予 **`retrieve` + `ingest`** 即可（不需要管理知识库、聊天或完整权限）。若表单支持 Knowledge Base 白名单，只勾选目标 KB。
3. 在 **Settings → RAG / External knowledge base**：选择 Provider `WeKnora`，填 Base URL（如 `https://<host>/api/v1`）、API Key、知识库 ID，点 **Test connection**。
4. 打开 **Enable RAG**，选择要同步的内容（正文 / 笔记 / 批注 / 图表索引 / BibTeX 引用），点 **Sync now**；之后可开启自动同步。

说明：

- 同步只上传文本：正文、笔记、批注文本、图表索引与 BibTeX 引用；**PDF 与图片二进制不会上传**。
- API Key 只保存在服务端 `settings.json`（0600），前端只显示掩码；日志与同步状态不含 key。
- 单 Vault 对应单知识库；更换知识库会重建映射。需要外部 MCP / Web Agent 使用 RAG 时，在设置里打开对应的工具开关。
- RAGFlow 等其他 provider 尚未实现。接口与实现细节见 [RAG 文档](docs/backend/rag.md)（进阶）。

## 远程访问与安全

- 默认只监听 `127.0.0.1`，适合本机或经 NAS 内网访问。
- 需要公网或跨网络访问时，请在自己的反向代理上加 TLS 与鉴权；WebSocket `/api/events` 必须支持代理透传（否则页面无法接收实时事件）。
- MCP 同样没有内置鉴权，不要裸暴露公网；建议仅在需要时开启，并用 `AGENTERO_MCP_ALLOWED_HOSTS` 限定允许的 `Host`。
- 文件接口默认只允许 Vault 与数据目录；确需访问其他路径时再用 `AGENTERO_WEB_ALLOW_ROOTS` 显式放行。

## 已知限制

- 单用户设计：不支持多租户、权限隔离或多 Vault 并发写入。
- 推荐部署环境为 Linux x86_64 + Docker；Windows、macOS、ARM 及大规模 Vault 尚未完整适配，建议先小范围试跑。
- Agent 当前以 Dsh 为主，其他 ACP Agent 需自行安装配置。版面解析依赖自建 MinerU 等服务，浏览器本地 ONNX 回退未在 Web 端完整验证。
- RAG：单 Vault ↔ 单知识库；图片语义检索、PDF 原件上传不在当前范围；WeKnora rerank 默认关闭（部分实例较慢）。
- MCP 无鉴权；Web 版不提供系统托盘 / 自动更新 / Zotero Connector / SSH 远程 Vault / LaTeX 编译 / 站点代理广场。

## 文档

- 部署与运维：[`docs/usage/web-self-host.md`](docs/usage/web-self-host.md)
- 用户指南（桌面与通用功能）：[`docs/usage/index.md`](docs/usage/index.md)
- MCP 工具与协议：[`docs/backend/mcp.md`](docs/backend/mcp.md)（进阶）
- RAG provider 与接口：[`docs/backend/rag.md`](docs/backend/rag.md)（进阶）

---

## License

MIT（见 [LICENSE](LICENSE)）。本项目是基于 Agentero 的衍生版本，原项目版权归其作者所有；本仓库的修改同样以 MIT 发布。
