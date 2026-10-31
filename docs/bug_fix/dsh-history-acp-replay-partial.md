# Dsh 会话标题显示系统提示、历史正文缺失

## 现象

使用 Dsh Agent 的会话历史时：

1. 列表标题是 `You are an assistant working` / `You are running the Agentero`
   这类系统提示开头，而不是用户的第一条问题。
2. 点开历史会话后只剩思维链（reasoning / Chain of Thought），用户提问与
   最终回答缺失。

## 原因

本机 Dsh 的 ACP profile 由 `@openma/deepseek-harness-acp` 提供（当前为
0.4.37）。与官方 `@deepseek-ai/dsh-acp` 的 resume-only 桥不同，它声明
`agentCapabilities.loadSession = true` 并实现 `session/load` 回放，因此
`history.rs::load_acp_session` 会走 ACP 回放，不再读取本地 durable log。

实际验证得到两类不完整回放：

- **正文缺失**：真实用户会话（`/vault`）的 `session/load` 回放中，每条
  `assistant/message` 只回放 reasoning + tool 事件，`agent_message_chunk`
  的正文为空，最终回答丢失；用户提问则是完整的。
- **标题为机器文本**：`session/list` 的 `title` 直接取 durable log 的
  `session/title` 事件。当标题来源是 `source.kind = "fallback"` 时，Dsh
  只是把首条 prompt 的前 ~30 个字符截断；Agentero 发出的首条 user message
  是 Host 的 system envelope（`build_prompt`），于是标题变成
  `You are an assistant working`。前端把任何非空 ACP 标题当作人类标题，
  `sessionsNeedingTitleHydration` 不会补全，列表永久显示机器文本。

## 修复

- Host：本机 Dsh（`template == Dsh` 且能解析 `DSH_HOME`）**优先**读取
  durable log（`sessions/<project>/<id>/session[.vN].jsonl[.zstd]`）；成功
  直接返回。日志缺失 / 损坏 / 自定义 persistence / 未来格式时再回退到 ACP
  `session/load` 回放。远端 Dsh 没有本地 home，行为不变。
  - 同时接受 `session-<uuid>` 形态的 Dsh 会话 id（Dsh CLI/Web 创建），
    ACP 创建的裸 UUID 行为不变。
  - durable log 的 `session/title` 事件若 `source.kind == "fallback"` 则忽略，
    交给前端从首条 user message 推导标题；`provider` 标题保留。
- 前端：新增通用机器标题识别 `isMachineHistoryTitle`（`prompt-display.ts`）。
  只匹配已知 system envelope 的前缀切片，或整句 prompt 开头，不误伤正常
  标题；多行正文交给 envelope 清洗。
  - `displayHistoryTitle` 不再把机器标题当标签（fallback 也过滤）。
  - `mergeImportedSessions` 不把机器 ACP 标题当成人类标题；先前持久化的
    机器标题也按占位符处理并进入 hydration 候选。
  - `sessionsNeedingTitleHydration` 把机器标题列为候选。
  - `history-title-cache` 读写都过滤机器标题，避免旧版本缓存继续占用列表。

## 验证

- `dsh_history` 单测：`session-<uuid>` id、fallback/provider 标题、压缩
  多轮正文、错误 workspace / 非法 id / 未知版本 / 损坏文件。
- 前端单测：机器标题识别（含防误伤）、`mergeImportedSessions` /
  `sessionsNeedingTitleHydration` / `hydrateSessionTitles` / 标题缓存过滤。
- 本机真实 Dsh session（`2f53dd13…`）通过 `agent_load_session` 返回
  durable 正文（user 提问 + 最终回答）且不再触发 ACP 回放；
  浏览器历史列表显示用户首条问题作为标题，点开正文完整。
- 真实用户部署（`/vault`）的 payload（`/tmp/user-session.json`）复现：
  修复前 `lines` 只有 reasoning；修复后同会话走 durable log。

## 兼容与风险

- 仅本机、标准 JSONL 存储的 Dsh 走 durable log；自定义 persistence root、
  v0/v1、未来格式与远端 Dsh 仍走 ACP，失败时行为与修复前一致。
- durable log 与 ACP 回放同时存在时以磁盘日志为准；正在运行中的会话由
  Dsh 增量落盘（`session-log-deepseek/delivery-accepted`），仍可读取已提交
  部分。
- 机器标题识别基于已知 system envelope 前缀；Agentero 新增 workflow
  envelope 时需要同步 `MACHINE_TITLE_PROMPTS`。
