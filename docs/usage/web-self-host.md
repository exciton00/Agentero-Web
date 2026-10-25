# 自部署 Web 版（headless）

Agentero 的 headless Web 宿主把同一个 Vault 与工作台界面跑在一台**常开的机器**上（NAS / VPS / 小主机），用浏览器访问。
单用户设计：**不内置登录**；如需远程访问，请自行负责 TLS / 鉴权 / 反向代理。

## 快速开始（Docker Compose）

前置：Linux + Docker Compose 插件。

```bash
git clone https://github.com/exciton00/Agentero-Web.git agentero && cd agentero

# 复制部署模板（实际文件不会进 git）
cp docker-compose.yml.example docker-compose.yml
cp .env.example .env
# 编辑 .env：Vault / 数据路径、端口、宿主机 Dsh 路径、可选 MCP 等
mkdir -p vault data

docker compose pull
docker compose up -d
# 打开 http://127.0.0.1:8787
```

默认使用已发布镜像 `ghcr.io/exciton00/agentero-web:latest`。需要自己构建镜像时，在 `.env` 设
`AGENTERO_IMAGE=agentero-web:local`，执行 `docker compose up -d --build`。

如需从原版 Agentero 迁移已有数据：先退出原应用，再把原 Vault 目录内容手动复制到
`AGENTERO_VAULT_DIR`，不要两个应用同时写同一个 Vault。当前版本不建议开启云同步，
Agentero 与 Agentero-Web 的同步状态/数据格式可能不兼容，混用可能产生冲突。

## 接入 Dsh Agent（宿主机挂载）

镜像**不包含** Agent CLI。Chat 面板使用宿主机上已安装并登录的 Dsh：compose 把宿主机 Dsh
包和 `~/.dsh` 挂进容器，Web host 启动时自动注册为正常的 `Dsh` catalog 行。

以 root + system npm 安装为例，`.env` 默认值：

```dotenv
AGENTERO_HOST_DSH_ROOT=/usr/lib/node_modules/@deepseek-ai
AGENTERO_DSH_HOME=/root/.dsh
```

对应 compose 挂载：

```yaml
- ${AGENTERO_HOST_DSH_ROOT}:/opt/host-agent/@deepseek-ai:ro
- ${AGENTERO_DSH_HOME}:/data/home/.dsh
```

若 `dsh` 通过 nvm / volta 安装，用 `readlink -f "$(command -v dsh)"`，把结果去掉
`/dsh/lib/bin.js` 后填入 `AGENTERO_HOST_DSH_ROOT`；`~/.dsh` 的路径填入 `AGENTERO_DSH_HOME`。
不需要挂载 `/usr/bin/dsh` 符号链接。

复用旧 `./data` 卷后如果看不到 Dsh（旧版本留下的自动注册标记），打开
**Settings → Agent →「检测宿主机 Dsh」→「重新检测」**，清除标记并重新注册即可。

**Vault 路径对齐**：容器内默认 `/vault`。若要求宿主机与容器路径完全一致，在 `.env` 同时设：

```dotenv
AGENTERO_VAULT_DIR=/srv/agentero/vault
AGENTERO_VAULT_CONTAINER_PATH=/srv/agentero/vault
```

其他 ACP Agent 需自行安装并登录；凭据由各自 CLI 持有。

## 环境变量（`.env`）

Docker Compose 的用户配置统一放在 `.env`（由 `.env.example` 复制），Compose 会自动读取；
`.env` 与实际 compose 文件都不进 git。

| 变量 | 默认 | 说明 |
|---|---|---|
| `AGENTERO_IMAGE` | `ghcr.io/exciton00/agentero-web:latest` | 镜像地址 |
| `AGENTERO_VAULT_DIR` / `AGENTERO_DATA_DIR` | `./vault` / `./data` | 宿主机 Vault / 应用数据（XDG）目录 |
| `AGENTERO_VAULT_CONTAINER_PATH` | `/vault` | 容器内 Vault 路径；与宿主机路径一致可让容器内外路径相同 |
| `AGENTERO_WEB_BIND` / `AGENTERO_WEB_PORT` | `127.0.0.1` / `8787` | 宿主机绑定与端口 |
| `AGENTERO_HOST_DSH_ROOT` | `/usr/lib/node_modules/@deepseek-ai` | 宿主机 Dsh 包根目录（挂载给容器） |
| `AGENTERO_DSH_HOME` | `${HOME}/.dsh` | 宿主机 Dsh 凭据 / profile 目录 |
| `AGENTERO_MCP_ENABLED` | 空 | `1`/`true` 强制启动 MCP listener |
| `AGENTERO_MCP_BIND` | 空 | MCP 监听地址，如 `0.0.0.0:8765` |
| `AGENTERO_MCP_ALLOWED_HOSTS` | 空 | MCP 额外允许的 `Host` 头（逗号分隔），反代域名必须列在这里 |
| `AGENTERO_WEB_MAX_BODY_MB` | `64` | `/api` 请求体上限（MiB，8–512） |
| `AGENTERO_WEB_ALLOW_ROOTS` | 空 | 文件端点额外允许的根目录（`:` / `,` 分隔） |
| `AGENTERO_SYNC_ACCESS_KEY` / `AGENTERO_SYNC_SECRET_KEY` | 空 | 可选，云同步凭据 |
| `DEEPSEEK_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | 空 | 可选，服务端 Agent CLI 凭据 |
| `RUST_LOG` | `info` | 日志级别 |

完整模板见仓库根目录 [`.env.example`](../../.env.example)。

## 数据布局与备份

| 内容 | 位置 |
|---|---|
| 文献与笔记 | 宿主机 `AGENTERO_VAULT_DIR`（容器内默认为 `/vault`；Markdown/PDF + `.agentero/catalog.sqlite`） |
| 设置 | `$XDG_CONFIG_HOME/agentero/settings.json` |
| 使用记录 / 同步 / RAG 状态 | `$XDG_DATA_HOME/agentero/` |
| 模型与缓存 | `$XDG_CACHE_HOME/agentero/` |

备份 = 拷贝 Vault 目录 + `data/`（XDG）目录。Vault 本身可被任何外部编辑器直接读取。

## 常用设置

- **Settings → Agent**：检测宿主机 Dsh；沿用旧数据卷时点「重新检测」。
- **Settings → 翻译**：选择翻译服务；整篇翻译的「正文字号」「标题字号」可调，支持恢复默认。
- **Settings → 版面解析**：选择版面 / 正文解析引擎（如自建 MinerU）、设置解析页数上限。
- **Settings → 同步**：git / S3 / WebDAV 云同步；当前版本不建议开启（见快速开始）。
- **Settings → 通用 → MCP server**：开关 MCP 监听端口。
- **Settings → RAG / External knowledge base**：配置外部知识库（见下）。

## MCP 客户端接入

设置 → 通用 → **MCP server** 打开后，服务端在独立端口（默认 `127.0.0.1:8765`）提供
Streamable HTTP MCP，端点 `http://127.0.0.1:8765/mcp`。

- 本机客户端（MCP Inspector、Codex 等）直接连接即可；应用内 Chat 面板走 ACP。
- 容器 / 远程：`AGENTERO_MCP_BIND=0.0.0.0:8765` 发布端口，`AGENTERO_MCP_ALLOWED_HOSTS=<外部域名>`
  放行 `Host` 头，并由反向代理加鉴权（MCP 本身无鉴权，不要裸暴露公网）。
- `paper_text_get` 默认关闭；设置 → 通用 →「对外暴露论文正文」打开后才返回 PDF 文本（正文会发给
  MCP 客户端）。
- 工具清单 / resources / 安全边界见 [后端 MCP 文档](../backend/mcp.md)。

## 外部知识库 RAG（可选）

设置 → RAG：选择 **Provider**（当前 WeKnora；RAGFlow 预留、coming soon），填 Base URL、API Key、
选择知识库，点「测试连接」后打开开关并同步。服务端把每篇论文渲染成一份 Markdown 同步到知识库
（正文 + 图表索引 + NOTES + 批注文本；**PDF 与图片二进制不上传**），检索结果带论文路径与
BibTeX `\cite{}`。

- API Key 只保存在服务端 `settings.json`，前端只显示掩码。
- 隐私：显式 opt-in；笔记、批注、图注文本会发送到你配置的外部知识库。
- 同步方式：手动「立即同步」（可取消、可查看进度与失败项），或按设置开启自动同步。
- 单 Vault 对应单知识库；更换知识库会重置映射。需外部 MCP / Web Agent 使用时，打开设置里的工具开关。
- 详细权限与配置步骤见 [README 的 RAG 章节](../../README.md#外部知识库-rag)。

## 版面分析与大文件

设置 → 版面解析 → 选择 **MinerU**、打开 **Self-hosted**、填 Base URL 与 API Key（BYOK，只写入服务器
`settings.json`）。之后新入库或手动重析的论文由服务端调用 MinerU 解析，写 `source/layout.json` +
`layout-index.json`；PDF 工具栏的 FIGURES 面板展示图 / 表 / 算法 / 公式。

- 版面解析页数上限（0 = 不限）：超过上限的 PDF 自动跳过版面解析，后端也会兜底拒绝。
- 重复打开同一篇论文时浏览器会复用缓存，不会反复下载整份 PDF；PDF 走文件接口，不经过 RPC。
- `/api` 请求体上限默认 64MB，可用 `AGENTERO_WEB_MAX_BODY_MB` 调整。

## 已知边界

- 单用户、无内置鉴权；不要直接把 8787 暴露到公网，远程访问请加反向代理 + 鉴权，并确保
  WebSocket `/api/events` 支持透传。
- Agent（ACP）使用宿主机安装的 Dsh / 其他 CLI；容器内不包含 Agent 运行时。
- 部署基线为 Linux x86_64 + Docker；Windows / macOS / ARM / 大规模 Vault 未完整验证。
- 版面分析依赖自建 MinerU 等服务；浏览器本地 ONNX 回退未在 Web 端完整验证。
- Docker 内使用宿主机 Dsh 包 + `~/.dsh`（Linux）；自定义安装路径通过
  `AGENTERO_HOST_DSH_ROOT` / `AGENTERO_DSH_HOME` 指定。
- Zotero Connector、远程 Vault（SSH）、自动更新、系统托盘在 Web 版不可用或不移植。
