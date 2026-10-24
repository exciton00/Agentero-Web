# WeKnora RAG 同步（Web host）

> 状态：P1 后端已实现（`feat/webui`，headless Web host）。方案见仓库外的
> `weknora-integration-plan.md`；本文件记录**已实现契约**与运维/排障要点。
> 桌面 Tauri 版暂未接入（设置页在 web 显示）。

把当前 Vault 的每篇论文渲染成**一份合并 Markdown 文档**，通过 WeKnora 的
「手工 Markdown 知识」API 写入指定知识库；原地更新、内容哈希增量、可取消、
可重建映射。**PDF 与图片二进制不上传**（图注/批注/alt 文本保留，可检索）。

## 设置（`settings.weknora`）

服务端 `settings.json`（与桌面同源 schema）；API Key 只在服务端保存，
`settings_get` 返回 `*` 掩码，掩码回写会保留旧值。

```jsonc
{
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

`normalize()` 会 trim 字符串并 clamp：`autoSyncMinutes` 1–1440、`timeoutMs`
1s–120s、`topK` 1–50、`concurrency` 1–4、`indexTimeoutSecs` 10–3600。

## 渲染格式（renderer v1）

单篇文档 = 以下小节按序合并（开关可关；空小节省略）：

1. 元数据头：title、paper id、Vault path、authors、year/venue/doi、tags、
   `BibTeX key`、`\cite{key}`。
2. `## Abstract`
3. `## Paper body`：`PAPER.md`；所有 `![](images/…)` 替换为
   `[figure on page N]`（保留 MinerU 图注文本，不上传像素）。
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

## 映射模型

每篇论文 1 个 WeKnora 手工知识 + `custom_metadata`（≤17 字段，全部字符串）：

| key | 内容 |
|---|---|
| `agentero_managed` | 固定 `"true"`（只有带此标记的文档允许更新/删除） |
| `agentero_vault` | `sha256(vault 绝对路径)[:16]`，防止多 Vault 互删 |
| `agentero_paper_id` / `agentero_path` / `agentero_title` | 论文身份 |
| `agentero_doi` / `agentero_year` / `agentero_tags` | 检索/过滤辅助 |
| `agentero_kind` | `"paper"`（预留） |
| `agentero_renderer` / `agentero_hash` | 渲染器版本、内容哈希 |
| `agentero_source_updated_at` | 学术侧更新时间（ISO） |
| `agentero_has_body` / `agentero_notes` / `agentero_marks` / `agentero_layout` | 小节是否包含 |
| `agentero_pdf_excluded` | 固定 `"true"`（PDF 未上传） |

本地同步状态**不在 Vault、不入 git、不含密钥**：
`$XDG_DATA_HOME/agentero/weknora/<sha256(vault)[:16]>.json`

```jsonc
{
  "schemaVersion": 1, "kbId": "…", "rendererVersion": 1,
  "lastSyncAt": "…",
  "docs": {
    "papers/…": {
      "knowledgeId": "…", "contentHash": "…", "bibtexKey": "…",
      "tagIds": ["…"], "status": "ready|indexing|failed",
      "syncedAt": "…", "lastError": null
    }
  }
}
```

换 KB 会重置映射（旧文档留在旧 KB，需在旧 KB 下清理）；文件丢失时用
`weknora_reconcile` 从 `custom_metadata` 重建。

## 同步引擎

`scan (catalog) → render → diff(create/update/skip/delete) → execute →
tags → index wait → delete`：

- **执行**：并发 `advanced.concurrency`（1–4）；`create` =
  `POST /knowledge-bases/:id/knowledge/manual` → `PUT /knowledge/:id`（metadata）
  → 标签；`update` = `PUT /knowledge/manual/:id` 整篇替换 → metadata → 标签。
- **标签**：catalog tags（过滤 `@zotero:*` / `#` 内部 tag）直接用原名创建/复用
  为 KB 标签，缓存 name→id，一次 `PUT /knowledge/tags`（`updates` map）整体
  替换；删除论文只解除关联，不删可能被其他文档使用的标签。
- **索引等待**：轮询 `GET /knowledge/:id`，`enable_status=enabled` 且
  `parse_status ∈ {completed, finalizing}` 视为可用；超过
  `indexTimeoutSecs` 标记 `indexing` 并留给 reconcile，不阻塞整体。
- **删除**：只删本地映射中不存在于 catalog 的文档（本引擎创建过）；reconcile
  的 prune 还会再次校验 `agentero_managed + agentero_vault`。非 managed 文档
  永不触碰。
- **逐篇持久化**状态：崩溃后可续跑；`force` 强制全量 update；`paper` 参数只
  同步单篇（这种运行不做删除）。
- **自动同步**（默认关）：Vault 写入事件后 **60s 防抖** 触发，另有
  **30 分钟兜底扫描**；同一时刻只允许一个同步任务，运行中到达的触发被跳过。

## RPC / 事件（Web host）

| RPC | 参数 | 返回 |
|---|---|---|
| `weknora_status` | — | `{enabled,configured,baseUrl,knowledgeBaseId,kbName?,sync:{total,synced,indexing,failed,pending,lastSyncAt,lastError,running,progress:{phase,done,total,current}},autoSync:{enabled,intervalMinutes},tools:{agent,mcp}}` |
| `weknora_test` | `{baseUrl?,apiKey?,knowledgeBaseId?}` | `{ok,kbName?,knowledgeCount?,embeddingModelId?,error?}`（省略/掩码用已存 key） |
| `weknora_list_kbs` | `{baseUrl?,apiKey?}` | `{kbs:[{id,name,knowledgeCount,description?,capabilities?}],error?}` |
| `weknora_sync_start` | `{force?,paper?}` | `{started,total,changed,skipped,deleted}`（异步，进度走事件；运行中 `started:false`） |
| `weknora_sync_cancel` | — | `{cancelled}` |
| `weknora_reconcile` | `{prune?}` | `{adopted,missing,orphans,deleted}` |
| `weknora_clear_mapping` | `{deleteDocs?}` | `{cleared,deleted}`（deleteDocs 需 UI 二次确认） |
| `weknora_search` | `{query,topK?,paper?,tag?,mode?,rerank?}` | `{hits:[{paper:{ref,path,title,tags,bibtexKey},knowledgeId,chunkType,score,content,section?,latex}],count,degraded?}` |

事件：

- `weknora:sync`：`{phase:"scan"|"render"|"sync"|"index"|"done",done,total,current?,created,updated,skipped,deleted,failed,errors:[{paper,error}],running}`；
  `current`/`errors[].paper` 为 Vault 相对论文路径（致命错误可能给 `"<sync>"`）。
- `weknora:changed`：完整 `weknora_status` data payload。

## WeKnora API 与限制

- 端点前缀 `<baseUrl>/api/v1`；认证 `X-API-Key`；统一信封
  `{success:true,data}` / `{success:false,error:{code,message}}`。
- 写入用 **manual Markdown**（可原地 `PUT`，文件上传会 409 去重且只能删后重传）。
- 测试实例的 API key scope **不允许 `knowledge_ids` 过滤**（403 code 1002）：
  单篇检索改为 KB-wide 检索后按映射本地过滤；P2 工具沿用此策略。
- 检索默认 `rerank.enabled=false`（该实例开启 rerank 可 25–40s 不返回）；
  请求超时后若无 rerank 结果会降级重试并标 `degraded`（工具层）。
- 索引是异步的：`finalizing` 可持续数十秒；worker 不阻塞，超时标 `indexing`。
- 检索结果**不含 custom_metadata**：`knowledge_id → paper` 映射只能靠本地状态
  或 `weknora_reconcile` 重建。

## 安全

- 显式 opt-in；开启即表示笔记/批注/图注文本会发送到外部知识库。
- API Key 仅服务端 `settings.json`（0600）；日志/RPC 错误/状态文件均不含 key。
- 只写 `agentero_managed` 且 `agentero_vault` 匹配的文档；删除/覆盖前都校验。
- 所有出站 HTTP 走 `agentero_core::http`（支持设置里的网络代理、TLS 校验、
  重定向上限、超时）。
- v1 单 Vault ↔ 单 KB；图片像素、PDF 原件、WeKnora 侧聊天/Agent 均不在范围。

## 已知缺口（P1）

- `sync.ensurePaperBody=true` 目前只是渲染时容忍缺 `PAPER.md`（文档标
  `agentero_has_body=false`）；尚未自动排队 parse-body 任务，P2 再补。
- 无图片/PDF 上传（需 KB 视觉模型）；无排除规则（private tag/dir，v1.1）。
- 论文移动/改名（id/path 变化）按新增+删除处理；同 id 移动会更新文档内容与
  metadata（path 变化参与 hash）。
- `weknora_search` 的 section 为启发式（按固定 H2 标题识别），chunk 跨节时可能
  标为最先命中的小节。
- 前端设置页/RAG 工具（MCP / web agent）不在 P1 后端范围。
