# MCP Server

桌面 Host 内嵌的 Streamable HTTP MCP。设置开关打开后在 loopback 监听；关掉即停。默认作用域是当前打开的**本地** Vault；`vault_list` 会列出用户打开过的本地 Vault，其它工具可用可选参数 `vault`（绝对路径）切换到其中任意一个。

应用内 Quick Chat / 侧边栏用的是另一条会话级 stdio MCP（`agentero-context`），只读本轮授权的来源，不复用这里的全局工具面。见 [context.md](context.md)。

远端 Vault 不服务。App 必须开着。

## 开关与地址

设置 → 通用 → **MCP server**。

| 设置 | 默认 | 说明 |
|---|---|---|
| `mcpEnabled` | `false` | 启停 listener |
| `mcpPort` | `8765` | 只绑 `127.0.0.1` |
| `mcpTunnelId` | `""` | OpenAI Secure MCP Tunnel ID（`tunnel_` + 32 hex） |
| `mcpTunnelApiKey` | `""` | Runtime（Restricted）API key；回显为 mask |

监听 URL：`http://127.0.0.1:{port}/mcp`。设置页端口旁绿点表示正在听；点击 URL 复制。

Host commands：`mcp_get_status` / `mcp_set_enabled` / `mcp_set_port` / `mcp_set_vault` / `mcp_set_known_vaults` / `mcp_set_parent_dir`，以及隧道相关 `mcp_tunnel_status` / `mcp_tunnel_start` / `mcp_tunnel_stop`。状态事件 `mcp:status`、`mcp:tunnel-status`。前端把 `agentero-recent-vaults` 同步给 `mcp_set_known_vaults`；`mcp_set_vault` 也会把当前 Vault 记入已知列表。

`initialize.serverInfo` 带 `title`、`websiteUrl` 和 `icons`（应用 PNG 的 data URI）。客户端可以忽略不画。

`initialize.instructions` 指向下方 resources，并强调 `paper_list` 默认瘦字段、覆盖 NOTES 前需确认。

无鉴权。不要把端口绑到非 loopback。

## Headless Web host（`agentero-web`）

Web 版把同一套 Streamable HTTP MCP 移植到 `crates/agentero-web/src/mcp/`（桌面 `src-tauri/src/integration/mcp/` 的实现不动），工具面、`initialize` instructions / icons、resources 与桌面一致；差异只在宿主接缝与隧道。

- **端点**：独立监听端口上的 `/mcp`（`POST` 与 Streamable HTTP `GET`），不复用 Web 主端口的 `/api` 路由。监听地址默认 `127.0.0.1:{settings.mcpPort}`（默认 8765），启动时按 `settings.mcpEnabled` 自动启停；开关/端口变化即时生效（重启 listener）。
- **环境变量**（容器 / 反代部署）：

| 变量 | 默认 | 说明 |
|---|---|---|
| `AGENTERO_MCP_ENABLED` | 空 | `1`/`true` 时无视 `settings.mcpEnabled` 强制启动 listener（容器里固定开启用） |
| `AGENTERO_MCP_BIND` | 空 | `host:port`，覆盖 bind 地址与端口（如 `0.0.0.0:8765`）。一旦设置，端口由 env 固定，`mcp_set_port` 返回 `lastError` 而不重启 |
| `AGENTERO_MCP_ALLOWED_HOSTS` | 空 | 额外允许的 `Host` 头（逗号分隔，如 `mcp.example.com`）。loopback 与 bind 主机自动允许；其它 Host 返回 403（DNS rebinding 防护） |

- **鉴权**：Web 版 MCP 同样**无鉴权**。绑定非 loopback 时启动日志会打 WARN；公网暴露必须由用户自己的反向代理做鉴权，并把外部域名加进 `AGENTERO_MCP_ALLOWED_HOSTS`。
- **状态**：`mcp_get_status` / `mcp:status` 的 payload 与桌面 `McpStatus` 一致：`{enabled, listening, port, url, lastError, vaultPath}`。`url` 仅在 listening 时给值；bind 为 `0.0.0.0`/`::` 时展示 `127.0.0.1`（wildcard 不可连接）。`mcp_set_enabled` / `mcp_set_port` 还会把 `mcpEnabled` / `mcpPort` 写回共享 `settings.json` 并广播 `settings:changed`，重启后保持。
- **端口切换**：换端口会先彻底关闭旧 listener（等待 serve 任务退出 + `SO_REUSEADDR`）再绑定新端口；目标端口被占用时返回 `lastError`（listener 保持停止），立即改回原端口或重试可一次成功，不会残留 `Address already in use`。端口由 `AGENTERO_MCP_BIND` 固定时 `mcp_set_port` 只回报错误、不改监听。
- **Vault 作用域**：服务端单 Vault（`AGENTERO_WEB_VAULT`，或前端通过 `mcp_set_vault` 选择的本地 Vault）。`vault_list` 返回该 Vault（`open=true`）+ `mcp_set_known_vaults` 同步的已知 Vault；其它工具的 `vault` 参数只能命中这些路径。
- **工具差异**：`import_id` 用进程内 `CapsCache` + Web host 的 `AppHandle` 发进度事件；`file_move` 用本地 WikiIndex 做链接重写（无渲染进程共享索引）；`file_delete` 无桌面 JobCenter 可取消（删除语义不变，仍默认回收站）；`paper_text_get` 每次调用读取实时 `settings.mcpExposePaperText`（opt-in 关闭时报错）。`layout_list` / `layout_get` / `page_read` 依赖 `source/layout.json|layout-index.json`，没有 sidecar 时返回 `layout_index_missing` 业务错误。
- **隧道**：Web v1 不含 ChatGPT Secure MCP Tunnel。`mcp_tunnel_status` 返回 `phase: "binaryMissing"` / `running: false` / `lastError` 说明；`mcp_tunnel_start` / `mcp_tunnel_stop` 返回业务错误。需要外网时由部署方用反向代理暴露 MCP 端点。

```bash
# 容器示例：Web 主端口 8787，MCP 独立端口 8765 对外发布（反代负责鉴权）
AGENTERO_WEB_ADDR=0.0.0.0:8787 \
AGENTERO_WEB_VAULT=/srv/agentero/vault \
AGENTERO_MCP_ENABLED=1 \
AGENTERO_MCP_BIND=0.0.0.0:8765 \
AGENTERO_MCP_ALLOWED_HOSTS=mcp.example.com \
  ./agentero-web
```

## ChatGPT Secure MCP Tunnel

App 开着且 MCP 开关打开后，设置区按界面顺序填写：先 **Runtime API key**，再 **Tunnel ID**，装好 `tunnel-client` 后点 **Start**。Agentero 随即 spawn 并持有 `tunnel-client run`。按钮旁绿点表示隧道已连通控制平面；**注意 `/readyz` 返回 200 不代表认证成功**，真正的 ready 信号是 `tunnel-client health --require-control-plane-poll` 的 `control_plane_poll.ok=true`。

隧道子进程随 Agentero 退出而停止（`RunEvent::Exit` 里 kill），设置区 **Stop** 与关闭 MCP 开关也会真正结束进程；宿主异常退出（崩溃、强杀）遗留的孤儿会在下次启动时按 `--profile-dir` 清扫。找不到 `tunnel-client` 时按钮禁用，并提示可复制安装命令 `brew install openai/tools/tunnel-client`，不会自动安装。该状态不是进程级缓存：`mcp_tunnel_status` 在未运行时重新 `resolve_command`，设置页在缺失期间每 2 秒拉一次状态，二进制出现后相位回到 `stopped`，不必重启应用。

Agentero 用独立 `--profile-dir`（`$XDG_CACHE_HOME/agentero/mcp-tunnel`）运行 tunnel-client，避免串到用户已有的 `~/.config/tunnel-client/*.yaml`；API key 只通过子进程 env `CONTROL_PLANE_API_KEY` 注入，不出现在命令行参数或 UI 日志。

Codex / Inspector 也可直接打 loopback URL。stdio 子进程不是这条通路。详细逐步教程见 [用 MCP 连接外部 Agent](../usage/mcp.md)。

## Resource

| URI | MIME | 内容 |
|---|---|---|
| `agentero://vault` | Markdown | 当前路径、schemaVersion、papers、unread，以及已知 Vault 列表 |
| `agentero://agent-invariants` | Markdown | 与 CLI/`agentero-core::ops` 同源的 agent invariants |
| `agentero://skills/agentero-cli` | Markdown | 当前平台 bundled `agentero-cli` Skill 正文 |

无 Vault 时 `agentero://vault` 仍列出，`resources/read` 返回「未打开 Vault」正文。`initialize` instructions 提示先读 vault → invariants，再调用 tools。

## Tools

`ref` = paper id 或 vault 相对路径（如 `papers/1706.03762`）。禁止 `..`。

每个 tool 都声明 `outputSchema`，成功时走 MCP `structuredContent`（ChatGPT 需要这份才能理解结果）。

| Tool | 作用 |
|---|---|
| `vault_list` | 列出用户打开过的所有本地 Vault（绝对路径、`name`、`exists`、`open`）。把返回的 `path` 作为其它工具的 `vault` 参数即可切换作用域；未出现在列表里的路径会被拒绝 |
| `paper_list` | 列表。**默认**每行只有 `id/path/title`（省 token）。`fields[]?` 按需加字段（`year`/`date`/`tags`/`authors`/`isRead`/…）；`full?` 恢复完整 metadata 行。另有 `query?`、`tag[]?`、`unread?`、`limit?`（默认 50，封顶 200）。abstract 只在 `paper_get`。 |
| `paper_get` | 单篇 metadata（含 abstract） |
| `paper_paths` | 相关路径：论文目录、`NOTES.md`、`PAPER.md`、`marks/`、本地 PDF、TeX 存在时的 `source/`；不读正文 |
| `paper_set_read` | 设置 catalog `isRead`（默认 true） |
| `import_id` | 魔棒入库（arxiv / DOI / URL）。`parent?` 默认当前 Library 作用域或 `papers` |
| `discover_arxiv` | 查询式发现：`keywords[]?` / `categories[]?` / `since?` / `until?` / `top?` / `maxCandidates?`，确定性词法排序，返回短名单（含 arXiv id，供后续 `import_id`）。`dedup?`（默认 true）剔除已在库的论文；无 Vault 时也可用（跳过去重）。不写 Vault |
| `paper_notes_get` | 读 `{paper}/NOTES.md`（文件不存在则空字符串） |
| `paper_notes_write` | 写 `NOTES.md`。`mode`: `replace`（默认）或 `append` |
| `paper_tag_add` | 加标签；可用 `topic:blue` 色后缀 |
| `paper_tag_rm` | 删标签 |
| `paper_tag_set` | 整体替换用户标签（保留内部 `@zotero:` / `@arxiv:` 标签）；增量修改仍用 add/rm |
| `paper_tag_list` | 全库标签清单（`name` / `color` / `count`）；`all?` 包含内部标签 |
| `paper_move` | 论文/组织目录移动到另一个 `papers/` 父目录，保留 basename；同 Vault 更新双链与 catalog，`toVault?` 时跨 Vault 迁移。重命名或非论文路径用 `file_move` |
| `paper_text_get` | **opt-in**（#676，默认关闭）：读取论文 PDF 的分页文本。`ref` + `pages?`（1-based，缺省全篇）+ `max_chars?`（每页字符预算，默认 20000、上限 50000）。需在设置中开启 `mcpExposePaperText`，未开启时调用直接报错；开启即意味着正文文本将发送给隧道另一端的外部客户端 |
| `layout_list` | 侧栏版面索引（需 `{paper}/source/layout-index.json`）。`kind[]?`、`minScore?` |
| `layout_get` | 按 region id 取一条（如 `figure-3`） |
| `page_read` | 按**物理页**读一篇论文正文（需 `{paper}/source/layout.json`，schemaVersion 3；桌面端跑过版面分析）。`ref` 必填，`page` 为 1-based 物理页。返回该页**阅读顺序**下的全部 region，外加 `textRegions`（段落级）；`pageCount` 为布局页数，越界页返回空数组而非报错 |
| `vault_search` | 只读全库 Markdown 关键词 AND 搜索（大小写不敏感，非语义检索）。`query` 必填；`limit?` 默认 60，限制到 1–200。根目录由当前作用域 Vault（`vault?`，缺省为打开的 Vault）决定，不接受任意客户端路径。返回 `hits`（相对 `path`、`snippet`、1-based `line`、`title`、`score`、可选 `paperPath`、可选 `page`）及 `truncated`；`PAPER.md` 正文命中会带上由 `source/layout.json` 映射出的 1-based 物理页码 `page`（无法唯一确定时省略）；命中用 `file_read {"path": hit.path}` 回读。可选元数据过滤 `year`（精确）、`publication` / `doi`（大小写不敏感子串）、`isRead`（bool）：一旦给出，只保留能映射到 catalog 论文且全部匹配的命中，其它一律丢弃 |
| `file_list` | 列一层目录。`path?` 为 Vault 相对路径，空则根目录。跳过 `.agentero`、隐藏目录和 LaTeX 编译产物。`limit?` 默认 200，最多 500 |
| `file_read` | 读一个 UTF-8 文本文件（如 `drafts/main.tex`、`notes/idea.md`）。不限 `papers/` |
| `file_write` | 写同一个路径。`mode`: `replace`（默认）或 `append`。父目录不存在会在 Vault 内创建。覆盖前需用户确认 |
| `file_move` | 移动/重命名 Vault 内一个文件或目录。`from` / `to` 都是 Vault 相对路径，目标必须不存在；同时更新可解析的 wiki 链接与 `papers/` catalog 路径。覆盖前需用户确认 |
| `file_delete` | 删除一个或多个 Vault 路径。默认移入回收站（`.agentero/.trash`，可撤销；`papers/` catalog 行随文件快照/恢复）；`permanent=true` 永久删除文件与 catalog 行。删除前需用户确认 |

除 `vault_list` 外，所有工具都接受可选 `vault`（绝对路径）：缺省 = 当前打开的 Vault；给值时必须命中 `vault_list` 返回的本地 Vault。

语义边界：

- `paper_move` 是论文库语义：参数是 `ref` + `toParent`，保留目录 basename，只能落到 `papers/`，可带 `toVault` 跨 Vault；`file_move` 是通用路径语义：`from` → 完整目标 `to`，可重命名，也可处理 `papers/` 外的文件。二者对 `papers/` 的 catalog/双链更新复用同一套 core 事务。
- `file_delete` 覆盖 CLI `paper delete` 的两种模式：默认回收站，`permanent=true` 对应 `--files`；不再单独提供 `paper_delete` 别名的工具。
- `paper_tag_set` 是替换，`paper_tag_add` / `paper_tag_rm` 是增量；三者共用 core 的标签写入函数，不会各自实现一套。

`vault_search` 复用 Host 搜索算法，不建索引；在遍历/读取前沿用 `file_read` 的路径过滤与符号链接边界。扫描大小写不敏感的 `*.md`（含 `PAPER.md` / `NOTES.md`），跳过隐藏/系统目录、`source`、超过 2 MiB 的文件；沿用深度 16、最多 20,000 文件的扫描上限。空白 query 返回空结果。`truncated` 仅表示匹配数超过 limit，不是扫描完整性或游标分页的保证。snippet 会去除常见 Markdown 前缀、最多约 200 字符；回读后用 line 定位原行。

`vault_search` 的 `page` 与 `page_read` 都是**启发式**的：`PAPER.md` 命中行与 `layout.json` 的 `text` region 做归一化（小写 + 折叠空白）后的等式/包含匹配，只有所有匹配 region 落在**同一个** `pageIndex` 时才返回页码，否则留空——绝不返回猜测。语义检索不在 MCP 范围（由外部索引承担），这里只补"页码语义 + 结构化元数据过滤"。

复现（macOS 未 stage 打包资源时可仅为测试设置 `TAURI_CONFIG='{"bundle":{"resources":[],"macOS":{"frameworks":[]}}}'`）：

```bash
cargo test -p agentero --lib vault_search_ -- --nocapture
cargo test -p agentero --lib page_read_ -- --nocapture
cargo test -p agentero --lib features::markdown::search -- --nocapture
cargo test -p agentero-core ops::tests
cargo test -p agentero-core pdf::layout_index
```

`vault_search_protocol_round_trip_reads_fixture_hits` 使用 `test/fixtures/mcp-search` 测试资料，实际运行内存 JSON-RPC 的 initialize、tools/list、tools/call 与 file_read 回读；不启动 listener，不代表 Streamable HTTP / App UI / Tunnel 的端到端验收。

`paper_notes_write`：

- 只写 `NOTES.md`，不动 `PAPER.md` / `source/` / `marks/`
- 原子写
- `replace`：新内容没有 YAML frontmatter 时保留原 aliases 头
- `append`：追加正文，保留 frontmatter
- 编辑器有未存改动时走现有 `vault:file-changed` 冲突逻辑

`file_read` / `file_write` / `file_move` / `file_delete`：

- 路径是 Vault 相对路径，禁止 `..`；符号链接解析后必须仍在 Vault 内
- 只接受 UTF-8 文本，单次最多 2 MiB。拒绝 PDF / 图片 / 压缩包等二进制扩展名，以及内容中的 NUL（`file_move` / `file_delete` 不读取内容，可移动/删除目录和二进制附件）
- 与文件树相同，拒绝 `.agentero`、`.` 开头的隐藏项（`.agents` 除外）、`target` 等忽略目录，以及 `.aux` / `.log` 等 LaTeX 编译产物
- 不写 `NOTES.md`（仍走 `paper_notes_write`，以保留 frontmatter）、`marks/annotations.json`、`source/layout-index.json`、`source/layout.json`、`catalog.sqlite`
- 原子写。编辑器打开同一文件时走 `vault:file-changed`
- `file_move` 复用 Host 的 wiki 重命名事务：先移动，再重写链接，失败会回滚；`papers/` 下的路径同步更新 catalog
- `file_delete` 默认进入 Vault 回收站（undoable）；`permanent=true` 时永久删除，调用方必须先取得用户确认

### RAG 工具（v2.1，动态）

当设置 → RAG 满足 `enabled && configured && provider 已实现 && tools.exposeToMcp` 时，tools/list **动态**多出 3 个只读工具（与 provider 无关，输出带 `provider`）；任一条件关闭后立即消失（同一 MCP 会话内也生效），`call_tool` 对未暴露工具返回可读的 tool-level error（`rag_disabled`）。其余 24 个工具完全不受影响。

| Tool | 作用 |
|---|---|
| `agentero_rag_search` | KB 语义+关键词混合检索（`mode`: hybrid 默认 / semantic / keyword；`rerank` 默认 false）。`query` 必填；`topK?` 1–50 默认 8；`paper?`（id/路径）限定单篇；`tag?` 按 catalog 标签过滤；`includeSummaries?` 默认 false（summary chunk 会去重）。返回 `{provider,providerLabel,query,hits,count,degraded?}`，每条 hit 含 `provider`、`paper{ref,path,title,tags,bibtexKey}`、`knowledgeId`、`chunkType`、`score`、`content`（≤4k 字符）、`section?`、`latex?`（`\cite{key}`）。 |
| `agentero_rag_paper_context` | 只查一篇已同步论文的正文/笔记/批注/图表索引：`paper` + `query` 必填，`topK?` 默认 6。返回 `{provider,providerLabel,paper,hits,count}`（hit 含 `section`）。实现上是 KB-wide 检索后按本地映射过滤（WeKnora API key scope 不允许 `knowledge_ids`）。 |
| `agentero_rag_sync_status` | 只读、无网络：`provider/providerLabel/enabled/configured/total/synced/indexing/failed/pending/lastSyncAt`；带 `paper?` 时返回该篇 `status/syncedAt/lastError/knowledgeId/bibtexKey`。 |

工具描述明确：适合"库里有没有讨论 X 的笔记/图注"这类跨文献问题；精读单篇仍用 `paper_get`/`page_read`；引用用返回的 `bibtexKey`/`latex`。命中内容按 `(knowledge_id, chunk)` 去重、单条裁剪到 4k 字符；provider 不可达/未启用返回可读 `isError`。

### Web Agent 的 RAG MCP 注入（P2）

Web host 在 `rag.enabled && configured && provider 已实现 && tools.exposeToAgent` 时，为每个 ACP turn 注入会话级 stdio MCP `agentero-rag`（`AgentHost::set_rag_mcp`，settings 保存后即时更新）：

- `prepare_run_turn` 写一个 **0600** 临时配置（baseUrl/apiKey/kbId/vault/statePath/检索默认值），把 `McpServerStdio("agentero-rag", current_exe, ["--agentero-rag-mcp", path])` 追加到 ACP `mcpServers`，并在 prompt 里插入一句短提示（仅在启用时；不写历史）。
- 该 turn 跳过 warm pool（暖会话无此 MCP server）；spec 变化（开关/KB/地址）时 `set_rag_mcp` 返回 changed 并清空 warm pool。
- 子进程 `agentero-web --agentero-rag-mcp <config>` 只讲 stdio MCP，暴露同一 3 个只读工具；config 在 turn 结束（guard drop）时删除，key 不落日志/命令行。
- 会话结束/失败也会清理；RAG 关闭时 ACP 工具面与历史行为一致（不注入、不提示）；provider/开关/KB 变化时 warm pool 失效。

不做：mark（请用 CLI）、把 PDF 当文本读、shell、stdio MCP。

机器契约与 CLI 共用 `agentero-core::ops`；CLI 侧用 `agentero describe` 自省。

## 代码

`src-tauri/src/integration/mcp/`：`McpController` + Streamable HTTP（`rmcp`）+ tools/resource。论文工具直接调 `features::{catalog, import, vault, pdf::layout_index}`；`file_*` 只做 Vault 内 UTF-8 文本读写。

## 安全

- 仅 `127.0.0.1`；默认关
- `ref` / `parent` / `file_*` 的 `path` 消毒，解析后的真实路径必须留在 Vault 内
- `vault` 只接受当前打开或前端同步过的本地 Vault 绝对路径；远程 handle、相对路径和任意目录都被拒绝
- 不暴露 PDF 二进制、不读 `.agentero` 与 XDG API key
