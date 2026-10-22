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
- 请求体上限：`/api` 路由统一用 `DefaultBodyLimit` 覆盖 axum 默认 2MB（默认 64MB，`AGENTERO_WEB_MAX_BODY_MB` 8–512 可调，非法回落 64）；`layout_remote_analyze_pdf` 的整份 base64 PDF、`POST /api/upload`、`PUT /api/file` 共用该上限。来源是 15 页 PDF 版面解析报 `length limit exceeded`。
- 缓存：`GET /api/file` 返回 `ETag` + `Last-Modified`，支持 `If-None-Match` / `If-Modified-Since` → 304；媒体 `private, max-age=3600`，JSON/Markdown 等 sidecar `private, no-cache`（必须 revalidate）；单区间 `Range` → 206。压缩：`tower-http` 的 gzip/br 只压文本/JSON/JS/CSS，PDF/图片等二进制不压（保留 `Accept-Ranges`）；`/assets/*` 带 hash 产物 `immutable`。
- 版面解析页数上限：`settings.layout.maxPages`（camelCase，默认 0 = 不限制，normalize clamp 0–10000）。前端在打开 PDF 后提前跳过超限文件；Web 后端 `layout_remote_analyze_pdf` 在解析 base64 后、上传前用 PDFium 页数兜底校验（PDFium 失败时退回 `/Type /Page` 计数），超限返回业务错误 `PDF has N pages, above the layout analysis limit of M (Settings → Layout)`。
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
- **服务端 runner**（Rust）：download assets / parse body / parse refs；`job:progress` 由 core 经 `HostHooks` 广播。
- **Renderer offer 协议**（浏览器执行）：`import` / `layoutAnalyze` / `citingScan` / `libraryIo` / `metadataRefresh` 默认走桌面 `renderer_offer_runner` 语义——
  1. 入队后任务立即置 `running`，服务端 emit `job:offer`（定向到发起标签；无标签上下文时广播），payload `{jobId,kind,vaultPath,paperPath,force,params}`；
  2. 前端执行器（`startJobCenterExecutorListener`）调用 `job_report{jobId,progress?,phase?,error?,state?}` 回报，服务端每次更新快照并 emit `job:changed`，终态额外 emit `job:completed` / `job:failed`；重复/迟到 report 幂等返回当前快照；
  3. 服务端等待终态：超时（默认 15 分钟，`AGENTERO_WEB_JOB_OFFER_TIMEOUT_SECS` 可调）→ `failed "renderer job report timeout"`；`job_cancel` 立即置 `cancelled` 并唤醒等待方；
  4. 浏览器重载后由 `job_list` 里 `state=running` 的 offer 重新认领（`claimRunningJobOffers`）。
- 无浏览器场景（headless/CI）可用 `AGENTERO_WEB_IMPORT_SERVER_FALLBACK=1` 让 `job_import_enqueue` 的 identifier 模式改走服务端 core import。
- `job_connector_sync_enqueue`（Zotero Connector）、`job_model_download` 等未移植：返回明确的 unsupported 业务错误。

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
- **Agent**：`crates/agentero-host`（20.6K LOC，0 tauri）+ web 侧 38 条 RPC 接入完成。**真实 Agent 浏览器实测通过**：把服务器上的 `dsh --profile acp`（真实 ACP CLI）注册为 agent → 面板显示模型/模式列表 → 中文提问「用一句话总结当前打开的论文」→ 真实模型流式返回中文总结（含 Chain of Thought 折叠、论文上下文注入）。fake ACP 亦验证了 `agent:status/stream/tool/usage/completed` 定向到发起标签。
- **导入**：`paper_import_local_pdf`（上传→识别→入库，含识别失败降级）、`paper_resolve_identifier`（arXiv/DOI）、`paper_import`（BibTeX）、`lookup_import_batch`、`paper_export`、ParseBody/ParseRefs 服务端任务（PAPER.md + agentero-cite.json 落盘）。
- **部署**：三阶段 Docker 镜像构建成功（119MB content，含 Node 22 + PDFium + tesseract/leptonica 预置），`docker compose up` 实测：容器 `/vault` 数据、浏览器 library/PDF/笔记正常；健康检查用内置 Node 探活；**容器内 MinerU 联通并完成 9 页/182 boxes 解析（6.2s）**。
- **设置入口**：web 构建恢复标题栏 Settings 齿轮（菜单 → Settings 打开独立设置标签页），并隐藏 Plaza/远程 Vault/Zotero 迁移等桌面专属入口；`builtin_provider_status` 在镜像中恒为不可用（BYOK-only）。

### 本轮验证（真实自建 MinerU）

- `layout_remote_probe` → `mineru 4.0.9`；`layout_remote_analyze_pdf`（1.2MB 论文 PDF）→ 9 pages / 182 boxes，约 6 秒；WS `layout-remote:progress`（uploading → running → downloading）payload 与 requestId 均匹配。
- 浏览器端到端：打开论文 → 版面分析 job 走 renderer offer → 前端 headless 执行器检测 `settings.layout.backend=mineru` → 服务端调用 MinerU → 写 `source/layout.json`（`source.mode="mineru-layout"`，183 regions）与 `layout-index.json` → PDF 工具栏「Analysis」打开 **FIGURES 面板（15 个图/表，缩略图 + caption + 页码跳转）**。
- 隐私：MinerU URL/key 只存在运行时 `settings.json`，仓库零泄露（提交前扫描通过）。
- **Vault 云同步（git 远程）**：`sync_configure`（backend=git、本地 bare repo）→ `sync_now` 首次 pass `uploaded=91`、version 1，裸仓库出现 `refs/heads/main` 提交（116 objects）；`sync_get_status`/`sync_disconnect` 正常。S3/WebDAV 走同一 core engine，凭据只存服务器 XDG `sync.json`。
- **renderer 任务协议**：`job_import_enqueue` 等改为 `job:offer`（定向标签）→ 浏览器执行 → `job_report`；WS 实测 offer→report→`job:changed`/`job:completed` 全链路；前端 Import 执行器与 layout 执行器已接入。
- **LaTeX / Zotero / 站点代理**：均按决策隐藏或返回 unsupported（`detect_latex_engines=[]`、`zotero_scan` business error、Plaza 区块在 web 隐藏）。

### 第二轮修复（2026-10-22）

| # | 问题 | 修复 |
|---|---|---|
| 1 | 设置里 Plaza / Zotero Connector 仍可打开 | web 下强制关闭并标注「Web 版不可用」；connector 状态不再发 RPC |
| 2 | 页面约 3 秒周期闪烁 | 根因：web 下自写文件事件回声（autosave PUT → watcher 广播回同标签 → 全量 refreshTree/读盘）。在 `file-change-gate` 丢弃本标签自写回声（TTL 2.5s），`plugin-fs` 写后登记路径；同场景全量树重建 51→1 |
| 3 | 设置页没有返回工作台 | 设置 shell 顶部加「← 返回工作台」 |
| 4 | 云同步面板不完整 | 三后端配置/状态卡/结果/冲突/范围字节+文件数；后端持久化 `lastOutcome`/`lastError`、`autoSyncNextRunAt`，并新增**服务端自动同步调度器**（autoSync + interval，与手动 sync 互斥） |
| 5 | MinerU 保存后绿灯不亮 | `probeLayoutProvider` / `runProbe` 的 `isTauri()` 早退改为 `hasHostTransport()`，且 self-hosted（无 key）也探测；实测绿灯 `可用` |
| 6 | 右键「在文件管理器中显示」 | web 下文件树/Library 菜单隐藏 Finder 项（终端项一并隐藏），快捷键面板同步隐藏 |
| 7 | Library 单击直接打开 | 单击=选中（Paper Info 跟随）、双击/Enter=打开、↑↓ 移动；右键新增「添加标签」「上传/覆盖 PDF」（上传后 `paper_rescan` 刷新；rescan 现会清 caps 缓存，`has_pdf` 即时更新） |
| 8 | 按标签筛选论文 | 新增隐藏缓存 `<vault>/.agentero/tag-index.json`（懒重建+写路径刷新）与 RPC `tags_index_read`；Library 顶部固定一行筛选条（左侧选中 chips + 清除，右侧可搜索多选下拉，色点+计数）；多选 AND、未选=全部、`#` 保留标签不进列表；选中 `papers/` 或子目录时同一行显示当前目录，自动叠加目录递归过滤 |
| 9 | 需要 bibtex/biblatex 网络挂载 | 设置→实验功能新增挂载开关/类型/范围（tag OR + 目录前缀，AND 组合）；开启后 `GET /agentero_online.bib` 实时输出（内容按挂载类型 bibtex/biblatex；本地序列化，**不经云端 translation-server**），`GET /api/bibtex/status` 供 UI 显示 |

### 第三轮修复（2026-10-22）

| # | 问题 | 修复 |
|---|---|---|
| 1 | 文献右键功能不足 / 状态不可见 | 新增「覆盖进行版面解析」「删除 PDF 文件」「删除版面解析文件」（与「删除」同组、红色警告色）；行尾新增 `PDF / 版面解析状态` 图标列：有 PDF 红色 PDF 图标、已解析蓝色自绘「md」方框、缺失灰色；数据来自新 RPC `paper_assets_status`；删除走新 RPC `paper_delete_assets(kind=pdf|layout)`（删后清 caps、刷 tag index） |
| 2 | Library 标签筛选交互不合理 | 删除左栏「按标签展开」虚拟节点；筛选并入 Library 单行：左侧 chips（可单删/清除），最右「标签筛选」下拉（搜索/多选/计数）；语义改为 **AND**；**未选 tag 不筛选（显示全部）**；与目录 scope 联动，行内显示「当前目录: papers/…」并递归列出+tag 过滤 |
| 3 | 侧边栏隐藏→显示后无法拖拽宽度 | `ResizableHandle` 常驻挂载（折叠态 disabled+width0，展开复位），新增 `expandAndResize()` 展开后 resize + 两帧断言；实测左右栏隐藏→显示后拖拽正常 |
| 5 | Library 多选缺失 | Ctrl/Cmd 单选切换、Shift 范围、Ctrl+A/Esc；多选右键批量菜单：批量添加标签 / 批量覆盖版面解析 / 批量删除 PDF / 批量删除版面文件 / 批量删除（二次确认带数量） |
| 4 | 左树单击文献直接打开 | 单击仅选中（高亮），双击/右键打开，目录单击仍展开 |
| 6 | `#` 开头保留 tag 出现在筛选 | `is_internal_tag_name` 视 `#…` 为内部 tag；`TAG_INDEX_VERSION` 1→2 强制重建缓存；前端再过滤一次 |
| 7 | 设置「返回工作台」新开一个工作台 | 改为 `window.close()` 关闭设置标签；仅当无法关闭（手动打开的标签）才回退导航到 `/` |
| 8 | 挂载路径变化 | BibTeX/BibLaTeX 统一挂载到 `GET /agentero_online.bib`（内容按挂载类型）；设置提示与 `/api/bibtex/status.path` 同步 |

### 第四轮修复（2026-10-22）

| # | 问题 | 修复 |
|---|---|---|
| 1 | 仍存在自动刷新 | 根因三层：服务端 watcher 对每个 OS 事件各发一帧（一次导入 96 帧）→ 前端逐帧刷新；任务阶段写盘无 burst 概念，阶段间静默期让三个 debounce 各自到期 → 3 波刷新；watcher/`paper:imported`/`paper:assets-ready`/`job:completed` 多来源互不合并。修复：watcher 同批合并为 1 帧 + WriteBurst 期间暂存、结束后合并（2.5s 兜底）；前端跨帧合并（`file-change-gate` 180ms/1.2s）+ 共享刷新协调器（`refresh-burst.ts`，三 store 统一延迟 flush）。实测一次导入 `vault:file-changed` 96→2-3 帧、刷新 3 波→2 波；外部单文件修改仍 1 帧/1 刷（~0.4s） |
| 2 | Vault 菜单不可用项 | web 下移除 打开知识库/打开远程/创建知识库/从 Zotero 迁移/与 Zotero 同步 及分隔线，保留最近 vault 列表 |
| 3 | 版面解析 body 超限 | `/api` 默认 body limit 2MB → 64MB（`AGENTERO_WEB_MAX_BODY_MB` 8..512）；新增 Layout 设置「版面解析页数上限」`layout.maxPages`（0=不限，clamp 0..10000），前端执行器超限直接跳过（job 成功 + 警告），后端兜底业务错误；实测 15 页 PDF 经 MinerU 解析成功（15 页/190 boxes） |
| 4 | 双击复制与打开冲突 | Library 单元格复制由双击改为**长按 ~550ms**（移动/离开/滚动取消，触发后吞掉 click，不选中不开论文；保留键盘/菜单复制）；双击行仍打开论文 |
| 5 | 流量与客户端缓存 | `/api/file` 增加 ETag/Last-Modified/If-None-Match/If-Modified-Since（304）、单区间 Range（206/416）、`Accept-Ranges`；媒体 `private,max-age=3600`，JSON/文本 `private,no-cache`；API/静态加 gzip/br 压缩；hashed 资源 immutable。实测首屏 ~252KB（br），重复打开 PDF 304（0B），layout.json 129KB→24KB（br） |

### 尚未完成 / 下一轮

- **魔棒入库的 renderer 任务**：桌面把 Import/LayoutAnalyze/CitingScan/LibraryIo/MetadataRefresh 通过 `job:offer` 交给前端执行；web 服务端目前只有 look-up 版本，导致浏览器魔法棒「按标识符导入」走 `job_import_enqueue` 后拿不到候选 picker。下一步在 TaskCenter 实现 `job:offer` → 浏览器 `job_report` 的 offer/claim/report 协议（前端执行器已存在），一次性打通魔棒、版面分析、引用扫描等 renderer 类任务。
- **真实 Agent CLI**：已用服务器上的 `dsh --profile acp` 完成真实模型端到端（浏览器中文问答）；镜像需按需安装 `claude`/`codex`/`dsh` CLI 并配置 API key（`doctor_check_host`/`doctor_install_node` 已可用）。
- **版面分析（ONNX）**：仍暂缓，等 renderer offer 协议落地后浏览器 wasm 可直接跑。
- 多文件串行识别（性能）、deferred RecognizeMetadata runner、其余 JobKind。

## 6. 产品决策（已确认，2026-10-21）

| 项 | 决策 | 实现状态 |
|---|---|---|
| 版面分析 | **需要**；采用**自建 MinerU**（server-side，v4.0.9 self-hosted）。URL/key 只存在运行时 `settings.json`（用户 BYOK），**不进仓库**。浏览器本地 ONNX 作为未配置远程时的 fallback | web 端在 `settings.layout.backend=mineru` 时由服务端调用 MinerU 并写 `layout.json`/`layout-index.json`（`layout_remote_analyze_pdf` RPC） |
| 站点代理（arXiv/Cool Papers/ModelScope 网页内嵌） | 不保留 | web 构建隐藏 Plaza 区块；相关 RPC 返回 unsupported |
| LaTeX 编译（rtex） | 不做 | `detect_latex_engines` 返回空，编译入口不显示 |
| Zotero 本地库迁移 | 不做 | `zotero_*` / welcome「Migrate from Zotero」在 web 隐藏/不支持 |
| 内置翻译 provider 凭证 | 不编入；只保留 BYOK / 免费引擎 | 镜像不注入 `AGENTERO_BUILTIN_*` 构建变量 |

## 7. 待确认 / 需要产品决策

1. **版面分析（ONNX）**：v1 暂缓是否可接受？（影响 Figures 侧栏、区域批注）
2. **站点代理/广场**：arXiv、Cool Papers、ModelScope 的网页内嵌阅读是否要保留？（需要服务端反代 + SSRF allowlist）
3. **Zotero 本地库迁移**（扫服务器上的 `zotero.sqlite`）：v1 是否需要？
4. **LaTeX 编译**（rtex）：服务端需装 TeX Live，镜像 +1GB；是否需要？
5. **Agent 安装方式**：镜像内置 Node + ACP adapters，用户自己装 claude/codex CLI（挂载或登录）——可接受吗？

## 8. 风险

- **Agent 移植**是最大块（22K LOC，WebviewWindow/AppHandle 解耦），也是"产品灵魂"，优先做技术验证。
- **写并发**：catalog 单写者；多标签同 Vault 写同一文件需靠现有保存冲突检查。
- **浏览器能力差异**：多标签、剪贴板、文件拖拽、下载；Chromium 优先，Firefox/Safari 后置。
- **部署依赖**：pdfium `.so`（liteparse）、Node 22 + ACP adapters、TeX（可选）、ONNX 模型（若启用）。
- **上游漂移**：`feat/webui` 需定期 rebase `origin/main`；settings 迁 core 是一次上游友好的重构。

## 9. 开发约定

- 命令名 / 参数 / 返回结构**必须**与 `src/lib/core/bindings.ts` 一致；新增命令先改前端契约。
- 事件名 / payload 必须与 `events_contract.rs` 一致（45 个）。
- 服务端 handler 统一签名：`async fn(args: Value, ctx: &RpcCtx) -> Result<Value, String>`，错误转 `{code,message}`。
- 前端垫片集中在 `src/web-transport/`；不改 `bindings.ts`（生成物）。
- 只读 Vault 相对路径 + jail 校验（复用 core `sanitize_vault_rel`）。
