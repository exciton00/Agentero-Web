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

## 二次修复（用户部署仍缺正文）

用户新部署已带上一版修复（durable log 优先、标题为 None/用户首句），但
`agent_load_session` 返回的 agent parts 只有 reasoning/tool、没有 text；最新
会话甚至只有一条 user line。分析：

- 用户会话的 durable `assistant/message` 事件确实不含 text block（两条
  独立实现：我们的 parser 与 ACP 桥的 `session/load` 回放都读不到正文）。
- 新版 DSH 会把**失败/中止的模型 attempt** 结算成 `assistant/attempt`
  事件，正文只存在于该事件的 `stream` 数组里（`text-chunks` +
  `block-end`），不会写入 `assistant/message`；对这类 attempt，此前 parser
  完全忽略，因此正文“消失”。
- 另外若 turn 在模型输出任何正文前就被中断（`turn/end reason=interrupted`），
  durable log 里本来就没有最终回答，任何客户端都无法恢复。

修复：

- `dsh_history`：解析 `assistant/attempt`，按 DSH 的 compact stream 规范
  （`chunk` 原始记录 / `text-chunks` / `reasoning-chunks` /
  `tool-call-chunks`，`block-end` 优先、未闭合块用累计 delta 作为可见前缀）
  重建 assistant 内容；按 `(turn, step)` 去重，completed
  `assistant/message` 优先，重试多次时取 streamed 文本最多的那次。
- `history.rs`：durable 正文完整时仍直接返回；当 durable 没有可见
  assistant 文本时，用有界超时（15s）再尝试一次 ACP 回放并**合并**：保留
  durable 的 user/tool/reasoning，只把 ACP 有而 durable 缺的 agent 文本行
  合入（durable 顺序/结构不变）。ACP 不支持、失败或超时都回退 durable。
- 前端：新增 `historyLacksSavedAnswer`；打开的会话若没有任何 agent 正文
  （只有 user/ reasoning/tool），在末尾追加 i18n 提示
  `messages.sessionAnswerNotSaved`（中英文），明确“没有保存最终回答，可能
  已中断，可展开 Chain of Thought 查看已保存推理”，不再看起来像空会话。

## 三次修复（用户“等待完成”的会话仍无正文）

用户在新构建（含 attempt 解析）上重测：`589d39d6…` 的
`agent_load_session` 仍只返回 reasoning + tool、无 text；最新会话只有 user
行。说明上一轮的 attempt 解析没有触达该日志，高概率是读取层更早失败或
settlement 结构不同。

本次加固（全部只读、保留安全校验）：

- 格式 generation：接受 v2–v6（`MAX_SUPPORTED_FORMAT`），v7+ 仍拒绝；
  header version 兼容数字/数字字符串，文件名兼容 `.jsonl.zst`。
- 目录/id 拼写：同时尝试裸 UUID 与 `session-<uuid>` 目录，并校验 header
  id、workspace、seq；响应仍返回请求的 session id。新增 flat
  `sessions/<id>` 与 `storages/sessions/**` 兜底布局。
- settlement 合并修正：之前“存在 assistant/message 就跳过 attempt”会漏掉
  “message 只有 reasoning/tool、正文在 attempt stream”的情形；现在按
  “该 message 是否有可见 text”决定，并把 attempt 的 text 合入同一 step 的
  message 位置（reasoning/tool 不重复）；同 step 多次 attempt 取文本最多者。
- content/stream shape：`message.content` 支持数组 / 数组嵌套 /
  字符串 / `text` 字段；stream 支持 `data.stream`、`data.message.stream`、
  `data.attempt.stream`；message 空 content 时从 stream 恢复。
- 未知 settlement 兜底：已知事件都取不到可见正文时，扫描未知
  `assistant/*` 事件里的文本（message/content/stream/text），去重后恢复，
  以兼容未来 generation 重命名事件。
- 诊断日志（INFO，默认可见）：每次 durable 读取打印
  `dsh history: restored path=… version=… events=… lines=… agent_text_chars=…
  merged_steps=… attempt_steps=… recovered_events=… event_types=…`；
  fallback/complement 路径也打印 INFO；header 不匹配时错误里带
  expected/actual id、version、cwd 匹配结果。`history.rs` 会打印 ACP 桥
  name/version 与 replay 行数。

## 四次修复（用户“等待完成”的会话根因确认：turn 被中断，而非 parser 漏正文）

用户把 `589d39d6…` 的 durable log 复制到 Vault 后，直接解压 29 条事件确认：

- assistant 的 settlement 只有 `reasoning` 和一个失败的 `bash` tool-call；
- 该 bash 失败原因是容器内没有可用的 workspace-write 沙箱后端：
  `sandbox mode "workspace-write" is requested but no sandbox backend is usable
  on this host; refusing to run the command unconfined`；
- 随后 `step/start` 了 step 2，但没有任何 `assistant/message` 文本，紧接着
  `step/end` / `turn/end { reason: { kind: "interrupted" } }`；
- 日志里不存在最终回答正文，因此历史加载正确显示了“未保存最终回答”。

这不是 durable parser 的问题；是 Dsh turn 在工具失败后/模型输出前被中断，
最终文本从未落盘。容器镜像原先没有安装 `bubblewrap`，Dsh 默认
`workspace-write` 无法启动 bash 沙箱，Agentero 的论文/工具类 turn 会在执行
工具时失败，进而出现这种无最终回答的会话。

修复：运行时镜像安装 `bubblewrap`，让 Dsh 的 `workspace-write` 沙箱在容器
内可用，避免正常的 bash/工具调用因“no sandbox backend”失败并连带中止 turn。
历史侧仍保留“未保存最终回答”提示；已经中断且未落盘的旧 turn 无法恢复。

### 沙箱后续

容器内安装了 `bubblewrap`，但 Docker 默认 seccomp 仍会阻止它创建 namespace
（`bwrap: Creating new namespace failed: Operation not permitted`）。实测只有
同时设置以下 Docker 选项才可用：

```yaml
cap_add:
  - SYS_ADMIN
security_opt:
  - seccomp=unconfined
```

为避免要求用户扩大容器 capability，运行时镜像默认设置：

```text
DSH_PERMISSION_MODE=danger-full-access
```

Dsh 官方说明该 preset 适用于 disposable container；Agentero-Web 是单用户
容器，vault 与 data 都在容器可写范围内。需要重新启用 workspace-write 的
部署可在 compose 中覆盖 `DSH_PERMISSION_MODE=workspace-write` 并加上上述
capability/seccomp 配置。

## 五次修复（Agentero 侧 transcript 缓存兜底）

四次修复确认 589d/6f 的最终回答从未落盘后，历史侧只能显示提示；但用户刷新前
**看到过**流式回答，该回答只存在于前端 transcript。本次为 Agentero 前端增加
浏览器侧 transcript 缓存：流式期间按 agent+session debounce 写入版本化
localStorage，刷新后打开历史时与 durable/ACP 结果合并；磁盘有正文时以磁盘为
准，缓存只补缺失的 assistant 正文或补回整轮丢失的回答。旧会话没有缓存时行为
不变（仍显示「未保存最终回答」）。设计与风险见
[中断会话的最终回答在刷新后丢失](agent-transcript-cache.md)。
