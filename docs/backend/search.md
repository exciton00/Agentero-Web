# 全文搜索

| Command | 说明 |
|---|---|
| `vault_search` | Vault 内 Markdown 等正文匹配 |
| `paper_search_content` | Headless Web Host 专用：按搜索域匹配 catalog 论文的用户内容（`NOTES.md` / `marks/` 高亮批注 / `source/layout-index.json` 图注），返回命中的 paper 路径 |

前端命令面板快速打开组合：内存论文列表（标题/作者/id/标签）+ `vault_search`（正文）+ `wiki_search`（frontmatter 别名，见 [../frontend/command-palette.md](../frontend/command-palette.md)）。

`paper_search_content({vaultPath, query, domains})` 的 `domains` 取 `notes` / `marks` / `captions`；query 按空白拆分后 AND、大小写不敏感（Unicode）。实现见 `crates/agentero-core/src/features/paper/search_content.rs`：进程级缓存按 `(vault, paper)` 保存抽取后的可搜索文本（NOTES、批注 quote/comment、图注 title），用 sidecar 的 `(mtime, size)` 失效；只遍历 catalog 论文，不向浏览器返回原文。

`vault_search` 代码：`src-tauri/src/features/search/`
