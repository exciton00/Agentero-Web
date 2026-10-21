# Headless Web UI（可自部署）

状态：**实施中**（`feat/webui` 分支，基线 `origin/main`）。M0 已跑通：浏览器打开 headless 服务端可以看到真实 Vault 文件树、论文库、PDF 阅读、NOTES 编辑与 Agent 面板。

目标：把 Agentero 变成一个**可自部署的 headless Web 服务**——部署在 NAS/VPS（优先 Docker Compose），浏览器访问完整工作台；数据仍在 Vault 目录（文件 + `catalog.sqlite`），服务端单用户运行。

## 1. 已确认的产品决策

| # | 决策 | 含义 |
|---|---|---|
| 1 | 部署到 NAS/VPS，优先 Docker Compose | 单一 Linux 二进制 + 镜像；也提供裸机/systemd 方式 |
| 2 | **单用户，不做内置鉴权** | 信任边界交给反向代理（Caddy/nginx/Tailscale）；服务默认可绑 127.0.0.1 或由 `AGENTERO_WEB_TRUST_PROXY` 说明 |
| 3 | **多标签必须**，**Vault 云同步必须** | 浏览器多标签页各自是独立工作区；S3/WebDAV 同步保留（`agentero-core::features::sync`） |
| 4 | Zotero Connector、远程 Vault（SSH）**不做** | 不移植 `integration/connector`、`integration/remote` |
| 5 | Agent 跑在服务器上（CLI + API key） | 保留 ACP/BYOA：服务端探测/安装/拉起 claude/codex，凭据存服务器 |
| 6 | **直接做 headless** | 不走"桌面进程当宿主"的过渡形态；新 crate 直接依赖 `agentero-core` |

**不做（v1）**：多用户/租户隔离、自动更新、系统托盘、原生菜单、Finder/终端集成、deep-link、桌面 App 探测、Bridge（iOS 配对宿主）。**暂缓（需产品确认）**：版面分析（ONNX）、Zotero 本地库迁移、站点代理广场（arXiv/ModelScope 网页嵌入）。

## 2. 架构

```text
浏览器（沿用现有 React SPA，14 个传输垫片）
   │  POST /api/rpc/<command>     命令（与 221 条 Host command 同名同参同返回）
   │  GET  /api/events            WebSocket 事件（45 个事件名不变）
   │  GET  /api/file, /api/upload  大文件（PDF/图片/附件）
   ▼
agentero-web（新 crate，axum 0.8）
   ├─ rpc 分发器（命令名 → handler）
   ├─ 事件总线（broadcast + 按标签页路由）
   ├─ 任务中心（服务端 Job，发 job:* 事件）
   ├─ Vault watcher（notify → vault:file-changed）
   ├─ 静态资源（SPA，dist/）
   └─ 各域 handler：settings / vault / paper / markdown / pdf / jobs / sync / agent
   ▼
agentero-core（已有，tauri-free）
   catalog / vault / import / wiki / marks / translate / sync / ops / usage
```

**关键接缝**：`agentero_core::app_handle::{AppHandle, HostHooks}` —— web 宿主实现 `HostHooks`（emit → WS，spawn → 任务中心），业务代码零改动。

### 2.1 Crate 布局

| crate | 说明 |
|---|---|
| `crates/agentero-core` | 已有。**本次把 `features::system::{settings,builtin}` 从 `src-tauri` 迁入**（tauri-free，桌面与 Web 共用同一份 `settings.json` 语义） |
| `crates/agentero-web` | **新增**。axum 服务端二进制 `agentero-web`；只依赖 `agentero-core` + axum/tokio/serde，不依赖 tauri。已实现 60+ 条 RPC（settings/vault/paper/trash/wiki/search/marks/download）+ 文件端点 + WS 事件 + watcher + 任务中心 + 云同步 |
| `crates/agentero-host` | **新增**。桌面 BYOA/ACP Agent 域的 tauri-free 移植（20.6K LOC，42 文件，0 tauri 依赖），`HostSink`/`Host` 三接缝抽象，`AgentHost` 门面覆盖 29 条命令 + 18 个事件；web 侧 handler 待接入 |
| `src-tauri` | 桌面壳保持可用（只改 settings/builtin 的 re-export） |

### 2.2 HTTP / WS 协议

- `POST /api/rpc/{command}`：body 为命令参数对象（camelCase，同 bindings.ts）。响应体 = 该命令原本的返回值：
  - `ApiResult<T>` 命令：HTTP 200，`{"ok":true,"data":…}` / `{"ok":false,"error":{"code","message","details"?}}`。
  - `Result<ApiResult<T>, String>` 命令（specta `typedError`）：成功 200 + 外层 `{status:"ok",data:…}`；IPC 级失败 500 + `{"status":"error","error":"…"}`。
  - 未知命令：404 `{"error":"unknown_command"}`。
- `GET /api/events`：WS。服务端 → 客户端 `{"event":"<name>","payload":…,"target":"<windowLabel|null>"}`。
  - 客户端 → 服务端：`{"type":"hello","windowLabel":"…"}`、`{"type":"emit","event":"…","payload":…}`（跨标签广播）。
- `GET /api/file?path=…&vault=…`：Vault 内文件字节（Range 支持），给 PDF/图片/附件用；前端 plugin-fs 垫片也可走它。
- `POST /api/upload`：multipart 上传（本地 PDF 导入、图片粘贴）。
- 静态：`/` → `dist/index.html`，`/assets/*` → 构建产物；SPA fallback。

### 2.3 多标签模型

- 每个 WS 连接 = 一个"窗口"：`windowLabel`（客户端生成，如 `tab-xxxx`）。
- 定向事件（`emit_to`）：服务端把事件打上 `target`，只投给该 label 的连接；未命中/无 target 的事件广播给所有连接。
- Agent 会话：`agent_run_once` 记录 `sessionId → windowLabel`，该会话的 `agent:*` 事件只发给发起标签。
- 前端：`getCurrentWebviewWindow()` 垫片返回带 `label` 的对象，`listen` 把 label 注册到连接上。
- Vault 会话：每个标签独立选择 Vault（`sessionStorage`），服务端按 (label, vault_root) 维护 watcher 引用计数。

### 2.4 任务模型

- 服务端任务中心替代桌面 JobCenter：`job:changed` / `job:completed` / `job:failed` / `job:progress` 事件契约不变。
- 默认服务端执行：import / download assets / parse body / parse refs / citing scan / metadata refresh / **sync**。
- `job:offer`（浏览器执行）仅用于版面分析（ONNX wasm）——**v1 暂缓**，前端收到 offer 时如实回报"不支持"。

## 3. 功能矩阵

| 能力 | v1 | 说明 |
|---|---|---|
| Vault 创建/打开/文件树/读写/重命名/删除/回收站 | ✅ | core 已有 |
| 论文库（列表/详情/标签/已读/元数据编辑/移动/删除） | ✅ | core catalog |
| 导入（标识符/本地 PDF/BibTeX） | ✅ | core import，服务端任务 |
| PDF 阅读（渲染/翻页/缩放/大纲/查找/划词） | ✅ | 前端 PDFium WASM；字节走 `/api/file` |
| 高亮批注 / marks | ✅ | core pdf marks + layout_index |
| Markdown 编辑 / 双链 / 反链 / 图谱 | ✅ | core wiki + 前端 Plate |
| 全文搜索 | ✅ | core wiki/search（Host MCP 逻辑搬入 core） |
| Agent（ACP：探测/对话/流式/权限/会话恢复） | ✅ | 新 crate `agentero-host` 移植 `features/agent`，去 WebviewWindow |
| Vault 云同步（S3/WebDAV） | ✅ | core sync engine |
| 设置 / 主题 / i18n | ✅ | core settings + 前端 |
| 多标签 | ✅ | 见 §2.3 |
| 版面分析 / Figures 侧栏 | ⏸ 暂缓 | 需 ONNX（浏览器 wasm 或服务端 ort） |
| Zotero Connector / 远程 Vault / Bridge / 更新器 / Finder·终端 | ❌ | 不移植 |
| 站点代理（arXiv/Cool Papers/ModelScope 网页嵌入） | ⏸ 视需求 | 先只保留 PDF 直连下载；网页代理后置 |

## 4. 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| **M0 骨架** ✅ | `agentero-web` crate：axum 启动、静态托管、RPC 分发器、WS 事件总线、`/api/file`；前端 transport 垫片；settings / vault 树 / 论文库 | 已完成：浏览器打开服务，渲染应用壳 + 真实 Vault 文件树 + 论文库（截图见 PR 描述） |
| **M1 只读闭环** ✅（写入联调中） | vault 文件读写、论文详情、PDF 阅读、笔记查看/编辑、搜索、watcher 文件变更事件、多标签 | 已验证：打开论文 → PDF 渲染（9 页/缩放）→ NOTES 编辑 → 论文库 1 行 → Agent 面板；设置页可读 |
| **M2 写闭环** | 笔记编辑保存、标签/已读/元数据、导入（标识符/PDF）、任务中心与进度、回收站 | 浏览器里完成"导入一篇 arXiv → 写笔记 → 打标签" |
| **M3 Agent + 同步**（进行中） | Agent 域已移植为 `crates/agentero-host`（20.6K LOC，0 tauri 依赖，229+7 测试通过）；云同步 RPC 已接 core（本地 bare git 仓库 e2e 通过）；剩余：32 条 `agent_*` RPC handler + sessionId→label 路由（约 2–3 人日） | 浏览器里与服务器上的 Agent 对话；S3/WebDAV 同步可用 |
| **M4 部署交付** | Dockerfile / compose、环境变量、健康检查、文档、备份说明 | `docker compose up` 在 NAS 上跑通全流程 |

## 5. 进度快照（2026-10-21，第二轮）

- **服务端**：`POST /api/rpc` 100+ 条命令（settings/vault/paper/trash/wiki/search/marks/import/jobs/sync/**agent 38 条**/doctor）、`/api/events` WS（定向路由 + 断连清理）、`/api/file|dir|upload|mkdir|rename`、多标签 watcher、服务端任务中心、云同步、静态托管、`main()` 前置 PDFium worker dispatch。
- **前端**：16 个传输垫片；`isTauri()` 分诊 158 处 → `hasHostTransport()`；服务端 Vault 在 web 启动时权威生效（不再被旧 localStorage 遮蔽）。
- **Agent**：`crates/agentero-host`（20.6K LOC，0 tauri）+ web 侧 38 条 RPC 接入完成；**浏览器实测**：Agent 面板列出服务器上的 agent（fake ACP），发消息后 `agent:status/stream/tool/usage/completed` 全部定向到发起标签，回复正常渲染。
- **导入**：`paper_import_local_pdf`（上传→识别→入库，含识别失败降级）、`paper_resolve_identifier`（arXiv/DOI）、`paper_import`（BibTeX）、`lookup_import_batch`、`paper_export`、ParseBody/ParseRefs 服务端任务（PAPER.md + agentero-cite.json 落盘）。
- **部署**：三阶段 Docker 镜像构建成功（170MB，含 Node 22 + PDFium），`docker compose up` 实测：容器 `/vault` 数据、浏览器 library/PDF/笔记正常；健康检查用内置 Node 探活。

### 尚未完成 / 下一轮

- **魔棒入库的 renderer 任务**：桌面把 Import/LayoutAnalyze/CitingScan/LibraryIo/MetadataRefresh 通过 `job:offer` 交给前端执行；web 服务端目前只有 look-up 版本，导致浏览器魔法棒「按标识符导入」走 `job_import_enqueue` 后拿不到候选 picker。下一步在 TaskCenter 实现 `job:offer` → 浏览器 `job_report` 的 offer/claim/report 协议（前端执行器已存在），一次性打通魔棒、版面分析、引用扫描等 renderer 类任务。
- **真实 Agent CLI**：镜像需按需安装 `claude`/`codex` CLI（或挂载），目前用 fake ACP 验证了全链路；`doctor_check_host/doctor_install_node` 已可用。
- **版面分析（ONNX）**：仍暂缓，等 renderer offer 协议落地后浏览器 wasm 可直接跑。
- 多文件串行识别（性能）、deferred RecognizeMetadata runner、其余 JobKind。

## 6. 待确认 / 需要产品决策

1. **版面分析（ONNX）**：v1 暂缓是否可接受？（影响 Figures 侧栏、区域批注）
2. **站点代理/广场**：arXiv、Cool Papers、ModelScope 的网页内嵌阅读是否要保留？（需要服务端反代 + SSRF allowlist）
3. **Zotero 本地库迁移**（扫服务器上的 `zotero.sqlite`）：v1 是否需要？
4. **LaTeX 编译**（rtex）：服务端需装 TeX Live，镜像 +1GB；是否需要？
5. **Agent 安装方式**：镜像内置 Node + ACP adapters，用户自己装 claude/codex CLI（挂载或登录）——可接受吗？

## 7. 风险

- **Agent 移植**是最大块（22K LOC，WebviewWindow/AppHandle 解耦），也是"产品灵魂"，优先做技术验证。
- **写并发**：catalog 单写者；多标签同 Vault 写同一文件需靠现有保存冲突检查。
- **浏览器能力差异**：多标签、剪贴板、文件拖拽、下载；Chromium 优先，Firefox/Safari 后置。
- **部署依赖**：pdfium `.so`（liteparse）、Node 22 + ACP adapters、TeX（可选）、ONNX 模型（若启用）。
- **上游漂移**：`feat/webui` 需定期 rebase `origin/main`；settings 迁 core 是一次上游友好的重构。

## 8. 开发约定

- 命令名 / 参数 / 返回结构**必须**与 `src/lib/core/bindings.ts` 一致；新增命令先改前端契约。
- 事件名 / payload 必须与 `events_contract.rs` 一致（45 个）。
- 服务端 handler 统一签名：`async fn(args: Value, ctx: &RpcCtx) -> Result<Value, String>`，错误转 `{code,message}`。
- 前端垫片集中在 `src/web-transport/`；不改 `bindings.ts`（生成物）。
- 只读 Vault 相对路径 + jail 校验（复用 core `sanitize_vault_rel`）。
