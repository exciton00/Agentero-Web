# RAG 集成（provider 抽象）

> 状态：v2.1（Web host）。WeKnora 是第一个已实现的 provider；RAGFlow 槽位已预留
> （调用返回可读 `not_implemented`）。方案见仓库外的
> `weknora-integration-plan.md` §9；本文件是**已实现契约 + provider 适配指南**。
> 桌面 Tauri 版暂未接入。

把当前 Vault 的每篇论文渲染成**一份合并 Markdown 文档**，经 provider 的
「手工 Markdown 知识」API 写入指定知识库；原地更新、内容哈希增量、可取消、
可重建映射。**PDF 与图片二进制不上传**（图注/批注/alt 文本保留，可检索）。

## Provider 抽象

| 层 | 位置 | 说明 |
|---|---|---|
| `RagProviderId` | `agentero-core/src/features/rag/providers/mod.rs` | `weknora`（已实现）、`ragflow`（预留，未实现）。带 `id()/label()/is_implemented()/capabilities()`，serde 为小写字符串。 |
| `RagCapabilities` | 同上 | `manual_markdown` / `in_place_update` / `tags` / `custom_metadata`。同步引擎按能力选择写入方式（不支持原地更新的 provider 走 delete+create 预留分支）。 |
| `RagBackend` | 同上 | enum dispatch：`WeKnora(WeKnoraClient)` / `RagFlow`。所有同步/检索/标签/删除调用都经过它；RAGFlow 分支统一返回 `not_implemented`。 |
| 共享 DTO | 同上 | `KbSummary` / `DocInfo`（带 `provider` + `providerLabel`），复用 `KnowledgeBaseInfo`/`KnowledgeInfo`/`SearchHit` 等 provider 返回结构。 |
| WeKnora adapter | `providers/weknora.rs` | 现 WeKnora REST client（base URL 归一、`X-API-Key`、KB/manual docs/metadata/tags/search/delete、错误分类、脱敏）。 |
| render/state/sync | `agentero-core/src/features/rag/` | provider 无关：渲染器、状态文件（按 provider 分文件）、同步引擎（`KnowledgeApi` 由 `RagBackend` 实现）。 |

### 新增一个 provider（以 RAGFlow 为例）

1. 在 `RagProviderId` 加 `RagFlow`（若尚未加），填 `id()/label()/is_implemented()/capabilities()`。
2. 新建 `providers/ragflow.rs`：客户端/HTTP 适配、错误映射（可参考 `providers/weknora.rs`，复用 `crate::http::client_with`），错误实现 `From<...> for AppError`；不要把 key 打进日志。
3. 在 `RagBackend` 增加 branch，并在 `providers/mod.rs` 的共享方法中 dispatch；`sync.rs` 的 `impl KnowledgeApi for RagBackend` 补 branch。
4. 若 provider 不支持原地更新：在 `sync::execute_doc` 用 `capabilities().in_place_update` 走 delete+create 预留分支（当前 WeKnora 全支持，不走该分支）。
5. 若 provider 不支持 custom_metadata/tags：同步引擎按能力跳过对应步骤（`custom_metadata` 仍会写但 provider 适配层可忽略/报错；勿在 v1 依赖）。
6. 加 mock/单测（KB/搜索/写入/错误映射 + 状态路径含 provider）并更新本文件。
7. `RagSettings.provider` 的 UI 选择已存在（前端 RAGFlow 显示 coming soon，选中后后端所有调用返回可读 `not_implemented`）。

## 设置（`settings.rag`）

服务端 `settings.json`（与桌面同源 schema）。v2.0 的字段名是 `weknora`，
`AppSettings.rag` 用 `#[serde(alias = "weknora")]` 一次性读取旧配置；写回只用
`rag`。API Key 只在服务端保存，`settings_get` 返回 `*` 掩码，掩码回写保留旧值。

```jsonc
{
  "provider": "weknora",          // weknora | ragflow（预留）
  "enabled": false,               // 显式 opt-in：笔记/批注文本会发送到外部 KB
  "baseUrl": "https://weknora.example.com",  // 可带/不带 /api/v1；仅 http(s)
  "apiKey": "",                   // 服务端保存，掩码回显
  "knowledgeBaseId": "",
  "sync": {
    "includePaperBody": true, "includeNotes": true, "includeMarks": true,
    "includeFigureIndex": true, "includeCitation": true, "ensurePaperBody": true,
    "autoSync": false, "autoSyncMinutes": 30
  },
  "tools": { "exposeToAgent": true, "exposeToMcp": true },
  "retrieval": { "rerank": false, "timeoutMs": 15000, "topK": 8 },
  "advanced": { "concurrency": 2, "indexTimeoutSecs": 180 }
}
```

`RagSettings::normalize()` 会 trim 字符串并 clamp：`autoSyncMinutes` 1–1440、
`timeoutMs` 1s–120s、`topK` 1–50、`concurrency` 1–4、`indexTimeoutSecs` 10–3600。
旧字段（v2.0）升级：读取时自动迁移；`settings_get` 只返回 `rag`，不再返回
`weknora`。`provider="ragflow"` 时 `rag_test`/同步/工具调用返回可读的
`not_implemented`，不会写 KB。

## 渲染格式（renderer v2）

单篇文档 = 以下小节按序合并（开关可关；空小节省略）：

1. 元数据头：title、paper id、Vault path、authors、year/venue/doi、tags、
   `BibTeX key`、`\cite{key}`。
2. `## Abstract`：catalog `abstract_text`；缺失时读 `metadata.json` 的 `abstract`。
3. `## Paper body`：优先 `PAPER.md`；缺失/为空时回退 `source/layout.json` 的
   paragraph/title/header 区域文本，再回退 PDF 文本层（PDFium）；所有
   `![](images/…)` 替换为 `[figure on page N]`（保留 MinerU 图注文本，不上传像素）。
4. `## Figure and table index`：`source/layout-index.json` 按
   `(page, title)` 去重；缺失时回退解析 `PAPER.md` 的 `Fig.` / `Table` 行。
5. `## User notes`：`NOTES.md` 去 YAML frontmatter，但 `aliases` / `created` /
   `tags` 保留为文本行；`![alt](assets/x)`、`./assets/x`、paper-reader 的
   `assets/figN.jpg` 统一改写为 `[笔记图片 alt (文件名)]`。
6. `## Annotations and marks`：`marks/annotations.json`（page/quote/note）与
   `marks/*.json`（visual/ask/translate 的 page/comment/image 文本）。
7. `## Notes images (binary not uploaded)`：笔记图片相对路径清单。
8. `## Citation (BibTeX / BibLaTeX)`：与 `/agentero_online.bib` **完全相同的
   key**（复用 `bibtex_export`：bibtex_key → 第一作者+年份 → paper id，重名
   `a/b/…` 后缀，mount tags/dirs 过滤）；bibtex + biblatex 两段 fenced entry；
   mount 未启用/不在范围时写警告行。

`contentHash = sha256(rendererVersion | 小节开关 | markdown)`：
`is_read` / `updated_at` 等非内容字段不参与；BibTeX key 变化会触发 update；
**渲染器版本升级 = 全量重同步**。单文档上限 300KB，超长截断并带标记。
v2.1 起 metadata 增加 `agentero_rag_provider`；旧文档缺该字段时按当前 provider
兼容（managed + vault + path 仍可映射），因此 v2.0 已同步的文档无需重建。

## 映射模型

每篇论文 1 个 provider 手工知识 + `custom_metadata`（≤20 字段，全部字符串）：

| key | 内容 |
|---|---|
| `agentero_managed` | 固定 `"true"`（只有带此标记的文档允许更新/删除） |
| `agentero_rag_provider` | `"weknora"` / `"ragflow"`（v2.0 旧文档缺失 = weknora） |
| `agentero_vault` | `sha256(vault 绝对路径)[:16]`，防止多 Vault 互删 |
| `agentero_paper_id` / `agentero_path` / `agentero_title` | 论文身份 |
| `agentero_doi` / `agentero_year` / `agentero_tags` | 检索/过滤辅助 |
| `agentero_kind` | `"paper"`（预留） |
| `agentero_renderer` / `agentero_hash` | 渲染器版本、内容哈希 |
| `agentero_source_updated_at` | 学术侧更新时间（ISO） |
| `agentero_has_body` / `agentero_notes` / `agentero_marks` / `agentero_layout` | 小节是否包含 |
| `agentero_pdf_excluded` | 固定 `"true"`（PDF 未上传） |

本地同步状态**不在 Vault、不入 git、不含密钥**：
`$XDG_DATA_HOME/agentero/rag/<sha256(vault)[:16]>.<provider>.json`

- 读取时若新路径不存在且 provider=weknora，会读取 v2.0 旧文件
  `$XDG_DATA_HOME/agentero/weknora/<vault>.json` 并一次性迁移；下一次保存写新路径。
- 文件含 `provider` 字段（v2.0 缺省=weknora）、`kbId`、rendererVersion、docs。
- 换 provider / 换 KB 都重置映射（旧 KB 文档留在旧 KB，需在旧 KB 下清理）。

## 同步引擎

`scan (catalog) → render → diff(create/update/skip/delete) → execute →
tags → index wait → delete`：

- **执行**：并发 `advanced.concurrency`（1–4）；`create` =
  `POST /knowledge-bases/:id/knowledge/manual` → `PUT /knowledge/:id`（metadata）
  → 标签；`update` = `PUT /knowledge/manual/:id` 整篇替换 → metadata → 标签。
  按 `RagCapabilities` 决定 manual markdown / in-place；不支持 in-place 的
  provider 预留 delete+create。
- **标签**：catalog tags（过滤 `@zotero:*` / `#` 内部 tag）直接用原名创建/复用
  为 KB 标签，缓存 name→id，一次 `PUT /knowledge/tags`（`updates` map）整体
  替换；删除论文只解除关联，不删其他文档可能用的标签。
- **索引等待**：轮询 `GET /knowledge/:id`，`enable_status=enabled` 且
  `parse_status ∈ {completed, finalizing}` 视为可用；超过
  `indexTimeoutSecs` 标记 `indexing` 并留给 reconcile，不阻塞整体。
- **删除**：只删本地映射中不存在于 catalog 的文档（本引擎创建过）；reconcile
  的 prune 还会再次校验 `agentero_managed + agentero_rag_provider + agentero_vault`。
  非 managed 文档永不触碰；v2.0 文档缺 provider 字段按 weknora 兼容。
- **逐篇持久化**状态：崩溃后可续跑；`force` 强制全量 update；`paper` 参数只
  同步单篇（这种运行不做删除）。
- **自动同步**（默认关）：Vault 写入事件后 **60s 防抖** 触发，另有
  **30 分钟兜底扫描**；同一时刻只允许一个同步任务，运行中到达的触发被跳过；
  provider 未实现/未配置时不调度。

## RPC / 事件（Web host）

| RPC | 参数 | 返回 |
|---|---|---|
| `rag_status` | — | `{provider, providerLabel, providerImplemented, capabilities:{manualMarkdown,inPlaceUpdate,tags,customMetadata}, enabled, configured, baseUrl, knowledgeBaseId, kbName?, sync:{total,synced,indexing,failed,pending,lastSyncAt,lastError,running,progress:{phase,done,total,current}}, autoSync:{enabled,intervalMinutes}, tools:{agent,mcp}}` |
| `rag_test` | `{baseUrl?,apiKey?,knowledgeBaseId?}` | `{ok, provider, providerLabel, kbName?, knowledgeCount?, embeddingModelId?, error?}`（省略/掩码用已存 key；provider=ragflow 返回 not_implemented） |
| `rag_list_kbs` | `{baseUrl?,apiKey?}` | `{provider, providerLabel, kbs:[{provider,providerLabel,id,name,knowledgeCount,description?,capabilities?}], error?}` |
| `rag_sync_start` | `{force?,paper?}` | `{started,total,changed,skipped,deleted}`（异步，进度走事件；运行中 `started:false`；provider 未实现/未配置返回可读错误） |
| `rag_sync_cancel` | — | `{cancelled}` |
| `rag_reconcile` | `{prune?}` | `{adopted,missing,orphans,deleted}`（兼容缺 provider 字段的旧文档） |
| `rag_clear_mapping` | `{deleteDocs?}` | `{cleared,deleted}`（deleteDocs 需 UI 二次确认；只删本 provider+本 vault managed 文档） |
| `rag_search` | `{query,topK?,paper?,tag?,mode?,rerank?}` | `{provider, providerLabel, hits:[{provider,paper:{ref,path,title,tags,bibtexKey},knowledgeId,chunkType,score,content,section?,latex}],count,degraded?}` |

事件：

- `rag:sync`：`{phase:"scan"|"render"|"sync"|"index"|"done",done,total,current?,created,updated,skipped,deleted,failed,errors:[{paper,error}],running}`；
  `current`/`errors[].paper` 为 Vault 相对论文路径（致命错误可能给 `"<sync>"`）。
- `rag:changed`：完整 `rag_status` data payload。

> 旧名已改：`weknora_*` RPC/事件/工具名在 v2.1 全部改为 `rag_*` / `agentero_rag_*`，
> 不提供 alias（避免两套契约并存）。

## MCP / Agent 工具（只读）

三个只读工具（`tools/list` 动态出现，需 `enabled && configured && provider 已实现 && tools.exposeToMcp`）：

| Tool | 作用 |
|---|---|
| `agentero_rag_search` | KB 语义+关键词混合检索（`mode` hybrid 默认；`rerank` 默认 false）；`paper?`/`tag?` 过滤；`includeSummaries?` 默认 false；返回 hit 含 `provider`/`paper`/`knowledgeId`/`chunkType`/`score`/`content`（≤4k）/`section?`/`latex?`。 |
| `agentero_rag_paper_context` | 只查一篇已同步论文的正文/笔记/批注/图表索引（`paper` + `query` 必填，`topK?` 默认 6）；返回 `{provider,providerLabel,paper,hits,count}`。 |
| `agentero_rag_sync_status` | 只读本地状态：`provider/providerLabel/enabled/configured/total/synced/indexing/failed/pending/lastSyncAt`；`paper?` 时带该篇 `status/syncedAt/lastError/knowledgeId/bibtexKey`。 |

- 关闭 `tools.exposeToMcp` 后同一 MCP 会话立即隐藏为 25 个工具；`call_tool` 返回
  可读 `{"code":"rag_disabled",...}`。
- Web agent（ACP）注入会话级 stdio MCP `agentero-rag`：host 写 0600 临时 config
  （`provider`/baseUrl/key/kb/vault/statePath/检索默认值），`prepare_run_turn`
  追加 `McpServerStdio` + 短提示词（仅开启时），turn 结束删除 config；spec 变化
  （provider/开关/KB）失效 warm pool。子进程
  `agentero-web --agentero-rag-mcp <config>` 暴露同一 3 个工具，MCP server 名保持
  `agentero-rag`。

## WeKnora 实现细节与限制

- 端点前缀 `<baseUrl>/api/v1`；认证 `X-API-Key`；统一信封
  `{success:true,data}` / `{success:false,error:{code,message}}`。
- 写入用 **manual Markdown**（可原地 `PUT`）；文件上传会 409 去重且只能删后重传。
- 测试实例的 API key scope **不允许 `knowledge_ids` 过滤**（403 code 1002）：
  单篇检索改为 KB-wide 检索后按映射本地过滤；`agentero_rag_paper_context`
  先取 8×topK 候选再按 knowledge_id 过滤，避免被其他论文挤占。
- 检索默认 `rerank.enabled=false`（该实例开启 rerank 可 25–40s 不返回）；
  请求超时后若无 rerank 结果会降级重试并标 `degraded`。
- 索引是异步的：`finalizing` 可持续数十秒；worker 不阻塞，超时标 `indexing`。
- 检索结果**不含 custom_metadata**：`knowledge_id → paper` 映射只能靠本地状态
  或 `rag_reconcile` 重建。
- 安全：显式 opt-in；API Key 仅服务端 `settings.json`（0600），日志/RPC 错误/
  状态文件均不含 key；只写 `agentero_managed` + 本 vault + 本 provider 的文档；
  所有出站 HTTP 走 `agentero_core::http`（代理、TLS、重定向上限、超时）。
- v1 单 Vault ↔ 单 KB；图片像素、PDF 原件、provider 侧聊天/Agent 不在范围。

## 已知缺口（v2.1）

- RAGFlow adapter 未实现（槽位/UI/错误语义已就绪）。
- `sync.ensurePaperBody=true` 目前仍不自动排队 parse-body 任务；但 renderer v2 会从
  `layout.json` / PDF 文本层补齐正文，避免只有 notes/figures 的文档。升级后需重新同步
  RAG（renderer version 变化会触发文档重渲染）。
- 无图片/PDF 上传（需 provider 视觉模型）；无排除规则（private tag/dir）。
- 论文移动/改名（id/path 变化）按新增+删除处理；同 id 移动会更新文档内容与
  metadata。
- `agentero_rag_search` 的 section 为启发式（按固定 H2 标题识别），chunk 跨节
  时可能标为最先命中的小节。
