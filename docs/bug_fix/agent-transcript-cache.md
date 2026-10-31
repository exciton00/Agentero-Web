# 中断会话的最终回答在刷新后丢失（浏览器 transcript 缓存兜底）

## 现象

用户等待 Dsh Agent 回答，界面上已经完整看到流式最终回答；刷新页面后从历史
列表重新打开该会话，正文却消失，只剩思维链（Chain of Thought）和工具调用，
底部还出现「没有保存最终回答（可能已中断）」提示。旧会话（如
`589d39d6…`、`6f9645b9…`）无法恢复。

## 根因

这是 Dsh durable log 侧的持久化丢失，而不是历史 parser 漏读：

- 两个真实会话的 durable log 中，`assistant/message` settlement 只有
  `reasoning` 和一个失败的 `tool-call`；随后 `step/start` 了下一步，但没有任何
  正文 `assistant/message`，紧接着 `turn/end { reason: interrupted }`。
- 也就是说最终回答从未落盘。刷新后 `agent_load_session` 只能返回 user +
  reasoning/tool，任何客户端都无法从磁盘恢复不存在的正文。
- 用户刷新前看到过完整回答，说明唯一副本在前端流式 transcript 里；前端没有
  持久化，刷新即丢失。

Dsh turn 被中断的上游诱因（容器缺少 workspace-write 沙箱后端、`bubblewrap`
在 Docker 默认 seccomp 下不可用等）已由运行镜像默认
`DSH_PERMISSION_MODE=danger-full-access` 缓解；但已经中断的 turn 与今后仍可能
发生的异常中止，需要客户端自兜底。

## 修复：Agentero 侧 transcript 缓存

新增 `src/lib/agent/transcript-cache.ts`：

- **存储**：版本化 localStorage key `agentero.agent-transcripts.v1`，按
  `agentId + (providerSessionId || runtimeSessionId)` 保存 `ChatLine[]`；
  不写入 Vault / catalog。
- **写入时机**：`useAgentTranscriptPersistence`（挂在 `useAgentPanel`）订阅
  Agent session store，流式 / 工具 / 推理 / 完成 / 错误更新都会进入
  debounce（500 ms 静默写一次；连续输出时硬上限 2 s）。`pagehide`、
  `visibilitychange(hidden)`、`beforeunload` 与面板卸载时同步 flush，避免
  「刚看到回答就刷新」时落在 debounce 窗口内。
- **容量与安全**：每会话最多 100 行（按完整 turn 对齐）、单行 200K 字符、
  单会话 1M 字符、总计 2M 字符、最多 40 个会话；超过上限按最旧条目淘汰，
  localStorage 配额失败时丢弃一半旧条目重试一次。composer 图片、PDF 视觉
  批注裁剪图、selection chip 等 base64/大字段在写入前剥离；工具 input/output
  超过 4K 字符丢弃。坏 JSON、错误版本、非法行全部按「无缓存」忽略。
- **恢复与合并优先级**（磁盘 / ACP 永远优先）：
  - `agent_load_session` 的 durable/ACP lines 有 assistant 正文时直接使用，
    缓存不覆盖；
  - durable agent 行缺少 text 时，按 agent 行序号用缓存行补齐 text，保留
    durable 的 reasoning / tool 顺序，并把中断时仍 pending 的 tool 状态从缓存
    快照补全；
  - durable 整轮丢失（只有 user 行、没有 agent 行）时，把缓存里该轮 user +
    回答追加到末尾（按 user 文本去重）；
  - 恢复的 agent 行强制 `streaming: false`、未完成 tool 标记失败，避免刷新后
    永久 spinner。
- **身份迁移**：turn 完成前缓存 key 是 Agentero runtime id（此时 provider id
  未知），`agent:completed` 学到 provider id 后把条目迁移到 provider key 并删除
  旧 key。若 turn 在完成前中断（provider id 永远学不到），打开历史时按
  **user 轮文本序列**匹配 provider 行与缓存条目（精确 key 未命中后的兜底），
  这是中断场景能恢复的关键。
- **清理**：provider 无法回放且没有可回放内容的历史行，若缓存还有 user 文本则
  保留该行并用缓存首条 user 生成标题，否则删除该行并清理缓存；历史列表刷新时，
  只有拿到**完整、无分页游标**的 provider 列表才清理「从列表中消失且本地无
  transcript」的会话缓存。运行中会话 / 本地有 transcript 的行永不清除。
- **UI 语义不变**：合并后仍有 assistant 正文则不再显示「未保存最终回答」；没有
  缓存的旧会话继续显示原有 notice。

## 验证

- `test/agent-transcript-cache.test.ts`（29 项）：缓存读写 roundtrip、坏 JSON /
  错误版本 / 非法行、图片与 base64 剥离、单行 / 单会话 / 条目数 / 配额容量
  淘汰、debounce 与硬上限 flush、runtime→provider key 迁移、合并优先级
  （磁盘有正文不覆盖、缺正文补齐、整轮追加、notice 保留）、刷新后按 user 轮
  匹配恢复、旧会话无缓存仍缺正文。
- `test/agent-history.test.ts`（新增 8 项）：`mergeCachedHistoryLines` 恢复中断
  回答、provider 正文优先、无缓存旧会话仍提示；`hydrateSessionTitles` 用缓存
  标题保留被 provider 清空的会话行并清理不可恢复行；`pruneCachedDeletedSessions`
  只在完整列表下清理、分页 / 运行中 / 本地 transcript 不清理。
- `pnpm typecheck`、`pnpm build:web`、`pnpm deps:check` 通过；
  `pnpm exec vitest run test/agent-*.test.ts test/history-title-cache.test.ts`
  307 项全部通过。

## 兼容与风险

- 旧会话（589d / 6f）没有缓存，无法恢复；notice 仍按 DSH 日志决定。
- 浏览器缓存只保存在当前浏览器 profile 的 localStorage：换浏览器 / 清站点数据
  即丢失；缓存与 Vault 无关，不参与云同步。
- 多个窗口（主窗口 + Agent 弹窗）使用同一 localStorage，整个 cache 是单 key
  读改写；并发写极端情况下可能互相覆盖（best-effort，不阻塞聊天）。缓存 key
  只在两个窗口都打开同一会话时才会冲突。
- provider id 无法学到时用 user 轮文本兜底匹配：若存在两个以上**不同**
  transcript 与参考 user 轮的重合度并列最高（例如两个会话都以「继续」开头），
  宁可放弃恢复也不猜，避免显示另一段会话的回答；只有唯一最佳候选（含
  runtime / provider 两份相同副本）时才会合并。兜底命中后，旧 runtime id
  条目会与新的 provider id 条目短暂并存（同一份 transcript 两份），由总容量
  淘汰回收；只有同一次刷新后的首轮打开可能出现。
- 历史列表分页（`nextCursor` 非空）时不做删除清理，避免误删仍在列表中但当前页
  之外的会话缓存；这些条目最终由容量淘汰回收。
