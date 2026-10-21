# Agent 域移植清单（BYOA/ACP → `agentero-host`）

状态：**第一阶段已落地**（`feat/webui`，基线 `origin/main`）。本文是 `docs/development/webui.md` §3「Agent（ACP）」的落地清单，对应里程碑 **M3**。

## 0. 结论（先读）

- 桌面 Agent 域 **22.8K LOC / 53 个文件**，其中 **15.6K LOC（37 个文件）完全不依赖 tauri**；全文只有 **67 行**出现 `tauri`（`grep -rn tauri`），集中在 **8 个命令壳文件（1.2K LOC，46 行）** 与 **8 个宿主文件（6.0K LOC，21 行）**。
- 真正的运行时耦合只有 **3 个接缝**：事件发射（`AgentEventEmitter`，26 个 emit 调用点）、后台 `spawn`（2 处）、宿主路径（`resource_dir`/`config_dir`，2 处）。其余 `AppHandle` 只作为「把 emitter 传下去」的参数出现。
- 本轮已把 **整个运行时**（models / registry / acp / runtime / session / prompt / citation / doctor / lifecycle）搬进 `crates/agentero-host`，**20.6K LOC / 42 个源文件**（含内联测试；不含 tauri 命令壳），`cargo tree -p agentero-host | grep tauri` 为空。
- `cargo test -p agentero-host`：**229 lib + 7 集成测试全部通过**（1 个需要官方 dsh 包的测试 ignored）。集成测试用 `node` 假 ACP 进程跑通 **initialize → session/new → session/prompt 流式 → cancel**，以及真实的 `run_once` / warm / session-list/load / probe 路径。
- **不建议新写最小 ACP 客户端**：ACP 传输/协议由 `agent-client-protocol 1.3.0` 承担，`acp/client.rs` 只是 launch/env/timeout 的宿主策略层；重写会丢掉 12 个内置模板、探测、warm pool、权限门控与 18 个事件的既有语义。详见 §8。

## 1. 度量与分层

| 层 | 文件 | LOC | tauri 行 | 处置 |
|---|---|---:|---:|---|
| **L0 纯逻辑** | `models` / `acp/*` / `runtime/*` / `session/*` / `prompt/*` / `context_turn` / `citation` / `registry/*` | 15 628 | 0 | **已搬**（只改 `crate::core::X` → `agentero_core::X`） |
| **L1 宿主接缝** | `runtime/events` / `session/warm` 的 spawn / `registry/bundled` 的 resource_dir / `registry/lifecycle`、`doctor`、`doctor_agents` 的 AppHandle | ~18 行 | 18 | **已替换**为 `HostSink` / `Host` |
| **L2 Tauri 命令壳** | `commands/*.rs`（8 文件） | 1 169 | 46 | **待 `agentero-web` 重写**（32 个 RPC handler，不在 host crate） |
| **L3 桌面专属** | `install/*`（桌面 CLI shim）、`remote_agent_*`（SSH 远程 Vault）、`integration::bridge` | ~1.5K | — | **不移植**（webui.md 决策 4/6） |

> 统计口径：`find src-tauri/src/features/agent -name '*.rs' | xargs wc -l` = 22 809 行；`grep -rn tauri` 命中 67 行 / 15 文件；`WebviewWindow|AppHandle|async_runtime|State<` 命中 94 行。差异来自 `State<` 与注释/路径中的 `tauri` 字样。

## 2. 模块级移植清单（✅ 已搬 / 🔁 已换宿主 / 🧱 待 web 重写 / ⛔ 不移植）

### 2.1 models / 契约（✅ 全搬）

| 模块 | LOC | 说明 |
|---|---:|---|
| `models.rs` | 890 | 全部 serde + specta DTO（`AgentDescriptor`、`AgentRegistryState`、`CatalogEntry`、`RunOnceRequest`、18 个事件的 payload、`WarmRequest`…）。仅 `specta(type = Option<crate::core::json::Json>)` 4 处改成 `agentero_core::json::Json`。 |

### 2.2 registry（agents.json / 内置适配器 / 安装 / 探测）

| 模块 | LOC | 处置 | 说明 |
|---|---:|---|---|
| `registry/store.rs` | 1 448 | ✅ | `agents.json` 读写/迁移/`scan_catalog`/`resolve_default`。新增 `AgentRegistry::load_in(dir)`；`load()` 保留桌面路径。 |
| `registry/templates.rs` | 707 | ✅ | 12 个内置模板 + 安装/登录命令 + `zcode_runtime_env`。 |
| `registry/discovery.rs` | 27 | ✅ | re-export `agentero_core::process::discover`。 |
| `registry/bundled.rs` | 550 | 🔁 | `init(app)` → `init(resource_dir: Option<PathBuf>)` / `init_from_dir`。manifest/PATH/env 注入原样。 |
| `registry/version_check.rs` | 385 | ✅ | npm latest 查询；`reqwest`/`semver` 已入依赖。 |
| `registry/antigravity.rs` | 450 | ✅ | 官方 `.par` 包下载/校验。 |
| `registry/dsh.rs` | 204 | ✅ | Windows npm 启动器解析（`cfg(windows)`）。 |
| `registry/lifecycle.rs` | 2 453 | 🔁 | **已全量搬**：8 处 `Option<&AppHandle>` → `Option<&dyn HostSink>`，进度事件广播；安装引擎（npm / 官方脚本 / zip 替换 / 协同取消）原样。 |
| `registry/remote.rs` | 286 | ⛔ | SSH 远程 Vault；`remote_agent_*` 3 条命令返回 unsupported。 |

### 2.3 acp（stdio JSON-RPC 客户端与事件映射）— ✅ 全搬

| 模块 | LOC | 说明 |
|---|---:|---|
| `acp/client.rs` | 1 283 | launch 策略：env 合并、PATH 解析、bundled tier、Unix `sh -c 'cd … && exec …'`、Windows `cmd /C`、超时预算（15s/30s/60s）。 |
| `acp/updates.rs` | 1 213 | **ACP `session/update` → 17 个 `agent:*` 事件的唯一映射层**。 |
| `acp/probe.rs` | 182 | `initialize` 探测 + 登录态诊断。 |
| `acp/interaction.rs` | 456 | permission / elicitation / ask-user 桥接。 |
| `acp/ask_user.rs` | 312 | Grok `x.ai/ask_user_question` 扩展方法。 |
| `acp/terminal.rs` | 720 | ACP `terminal/*` 服务端实现。 |
| `acp/session.rs` | 480 | **新增**：最小可运行单轮驱动（initialize → new/resume/load → prompt 流式 → cancel），作为 web 端「不接 warm 也能跑」的验证与兜底。 |

> 传输由 ACP SDK 的 `McpServerStdio` 拉起子进程并按行做 JSON-RPC 帧；`session/prompt`、`session/cancel` 都是 `ConnectionTo<Agent>` 上的类型化请求/通知。

### 2.4 runtime / session / prompt（✅ 全搬）

| 模块 | LOC | 说明 |
|---|---:|---|
| `runtime/events.rs` | 26→100 | `AgentEventEmitter { sink: Arc<dyn HostSink>, target }`，保留 `emit(event, payload)` 签名，26 个调用点零改动。 |
| `runtime/gates.rs` | 144 | 权限 / elicitation / ask-user oneshot 门控。 |
| `runtime/control.rs` | 169 | `AgentRunController` + `AgentWarmGate`。 |
| `runtime/stream.rs` | 229 | 60ms 合帧器。 |
| `session/run.rs` | 1 215 | 单轮编排：prompt 组装 → warm pool → initialize → new/load/resume → prompt → 流式 → `agent:completed`。 |
| `session/pool.rs` | 574 | warm 连接池（`HashMap` + `watch` + `CancellationToken`）。**无 tauri 依赖**。 |
| `session/warm.rs` | 516 | `tauri::async_runtime::spawn` → `tokio::spawn`（1 行）。 |
| `session/handlers.rs` | 255 | 连接 handler 装配（cold + warm-idle）。 |
| `session/history.rs` + `dsh_history.rs` | 1 181 | `session/list`、`session/load` 回放重建。 |
| `session/config.rs` | 294 | 模型/协作模式/effort/fast-mode 偏好。 |
| `prompt/{envelope,skills}` + `context_turn` | 973 | 提示词信封、SKILL.md、上下文 MCP 注入。 |

### 2.5 宿主 / 服务 / 诊断

| 模块 | LOC | 处置 | 说明 |
|---|---:|---|---|
| `host.rs` | 320 | 新增 | `HostSink` / `Host` / `MockSink` / `MockHost` / `LocalHost` |
| `service.rs` | 520 | 新增 | `AgentHost` 门面：registry / probe / run_once / warm / list / load / cancel / gates / lifecycle；web RPC 1:1 调用 |
| `doctor.rs` | 947 | 🔁 | `Option<&AppHandle>` → `Option<&dyn HostSink>`；auth 诊断 + Node 安装。 |
| `doctor_agents.rs` | 461 | 🔁 | `&AppHandle` → `&dyn HostSink`；并发复诊全部 agent。 |
| `citation.rs` | 737 | ✅ | `agent_resolve_citation` 的解析逻辑（PDF 坐标）。 |
| `install/*` | 1 428 | ⛔ | 桌面 CLI（`agentero-cli`）shim 安装/升级；服务器无意义。 |
| `commands/*.rs` | 1 169 | 🧱 | 35 个 `#[tauri::command]` 壳；由 `agentero-web` 的 `rpc` 分发器重写为 32 个 handler（每个 3–15 行）。 |
| `remote_host.rs` | 66 | ⛔ v1 | 仅保留精简 `RemoteAgentLaunch` trait 供 ACP client 编译（去掉 Vault 耦合的 `ensure_vault_skills`）。 |

## 3. tauri 依赖的具体位置与替换策略

### 3.1 运行时接缝（已替换）

| 位置 | 原代码 | 替换 |
|---|---|---|
| `runtime/events.rs:2,6,11,18` | `AppHandle` + `emit_to(EventTarget::webview_window(label))` | `HostSink::emit(event, payload, Some(label))` |
| `session/warm.rs:165` | `tauri::async_runtime::spawn` | `tokio::spawn` |
| `service.rs:142` | `window: &tauri::WebviewWindow` | `AgentHost::accept_run_once(target)` |
| `service.rs:204,224` | `window.app_handle().state::<…>()` + spawn | `AgentHost` 字段 + `Host::spawn` |
| `registry/bundled.rs:55` | `app.path().resource_dir()` | `Host::resource_dir()` |
| `registry/lifecycle.rs:436,504,524,641,1238,1413,1445,1490,1498` | `Option<&AppHandle>` + `app.emit` | `Option<&dyn HostSink>` + 广播 |
| `doctor.rs:205,709` / `doctor_agents.rs:338` | `Option<&AppHandle>` | `Option<&dyn HostSink>` |

### 3.2 命令壳（待 web 重写，不搬）

`commands/{registry,session,doctor,interaction,remote,citation}.rs` + `install/commands.rs`：35 处 `#[tauri::command]`、44 处 `State<'_, T>`、3 处 `WebviewWindow`、8 处 `Manager`。这些是「参数解包 + 调用 service + 错误映射」，web crate 的 `rpc` 分发器可 1:1 复刻。

### 3.3 明确不搬

`install/`（CLI shim、Windows PATH 注册）、`doctor_open_agent_login_terminal` / `doctor_open_agent_cli_terminal`（桌面终端）、`remote_agent_*` / `registry/remote.rs` / `RemoteAgentHosts`（SSH 远程 Vault）、`integration::bridge`（iOS 配对宿主）。对应命令在 web 端注册为「unsupported」stub，前端已有 `ApiResult` 错误分支。

## 4. 事件契约（18 个）

全部 payload 类型已存在于 `models.rs` / `acp::{ask_user,interaction}`。`target` 列：
- `tab` = 桌面 `emit_to(webview_window(label))`，web 端按 `sessionId → windowLabel` 定向投递；
- `all` = `app.emit` 广播。

| # | 事件名 | payload（Rust） | emit 位置 | target |
|---|---|---|---|---|
| 1 | `agent:registry-changed` | `{}` | `service.rs` / `agent/service.rs:40` | all |
| 2 | `agent:ask-user-request` | `AskUserRequestEvent` | `acp/interaction.rs:335` | tab |
| 3 | `agent:elicitation-request` | `ElicitationRequestEvent` | `acp/interaction.rs:377` | tab |
| 4 | `agent:permission-request` | `PermissionRequestEvent` | `acp/interaction.rs:437` | tab |
| 5 | `agent:models` | `AgentModelsEvent` | `acp/updates.rs:562` | tab |
| 6 | `agent:collaboration` | `AgentCollaborationEvent` | `acp/updates.rs:565` | tab |
| 7 | `agent:effort` | `AgentEffortEvent` | `acp/updates.rs:568` | tab |
| 8 | `agent:fast-mode` | `AgentFastModeEvent` | `acp/updates.rs:571` | tab |
| 9 | `agent:commands` | `AgentCommandsEvent` | `acp/updates.rs:630` | tab |
| 10 | `agent:tool` | `AgentToolEvent` | `acp/updates.rs:643,659` | tab |
| 11 | `agent:plan` | `AgentPlanEvent` | `acp/updates.rs:683` | tab |
| 12 | `agent:usage` | `AgentUsageEvent` | `acp/updates.rs:692`、`session/handlers.rs:102` | tab |
| 13 | `agent:session-info` | `AgentSessionInfoEvent` | `acp/updates.rs:720` | tab |
| 14 | `agent:failed` | `AgentFailedEvent` | `session/run.rs:164` | tab |
| 15 | `agent:status` | `AgentStatusEvent` | `session/run.rs:594`、`service.rs:204` | tab |
| 16 | `agent:stream` | `AgentStreamEvent` | `session/run.rs:305` | tab |
| 17 | `agent:completed` | `AgentResultPayload` | `session/run.rs:409` | tab |
| 18 | `agent-lifecycle:progress` | `{taskId,phase,downloadedBytes,totalBytes?,progress?}` | `registry/lifecycle.rs:1498` | all |

落地方式：
- `agentero-host` 只产 ``(event_name, serde_json::Value, Option<target>)``，**不知道 WebSocket**。
- `agentero-web` 的 `EventBus` 实现 `HostSink::emit`：`target=None` → 广播所有连接；`Some(label)` → 只投给该 label，并登记 `sessionId → windowLabel`。
- `AgentHost::accept_run_once(target)` 已把 target 贯穿整轮（含后台 spawn），web handler 只需传入当前标签。
- 契约防漂移：`tests/fake_acp.rs` 对真实 emit 的事件名/字段做断言；后续可在 web 端复用 `events_contract.rs` 的名字清单。

## 5. 命令契约（32 条）

> 任务书写「33 条」；按 `handlers.rs` 实际注册与 `bindings.ts` 逐条核对为 **32 条**（29 条 `agent_*`/`doctor_open_agent_*`/`remote_agent_*` + 3 条 `doctor_*`）。若把 `cli_install_*` 算入则为 35，但那是 CLI 域。

| 组 | 命令 | host 落点 | web handler |
|---|---|---|---|
| registry (15) | `agent_list_agents` `agent_list_skills` `agent_scan_catalog` `agent_check_catalog_updates` `agent_upsert_agent` `agent_ensure_catalog` `agent_remove_agent` `agent_set_default` `agent_set_user_agent` `agent_probe` `agent_probe_catalog` `agent_lifecycle_cancel` `agent_run_tool_lifecycle` `agent_run_partial_uninstall` `agent_tool_uninstall_info` | `AgentHost` | 直调（lifecycle 走 `spawn_blocking`） |
| doctor (5) | `doctor_check_host` `doctor_install_node` `doctor_check_agents` | `AgentHost` | 直调（安装走服务端任务） |
| | `doctor_open_agent_login_terminal` `doctor_open_agent_cli_terminal` | ⛔ | unsupported |
| interaction (3) | `agent_respond_permission` `agent_respond_elicitation` `agent_respond_ask_user` | `AgentHost` | 直调（多标签按 requestId 定位） |
| run (5) | `agent_run_once` `agent_list_sessions` `agent_load_session` `agent_cancel_run` `agent_warm` | `AgentHost` | 直调 + `sessionId→label` 登记 |
| citation (1) | `agent_resolve_citation` | `AgentHost::resolve_citation` | 直调 |
| remote (3) | `remote_agent_scan` `remote_agent_probe` `remote_agent_open_install_terminal` | ⛔ | unsupported |

参数/返回结构必须与 `src/lib/core/bindings.ts` 一致（camelCase；`ApiResult<T>` 的 `{ok,data|error}` 包装由 web `rpc` 统一处理）。

## 6. `agentero-host` 宿主抽象（已落地）

```rust
pub trait HostSink: Send + Sync {
    fn emit(&self, event: &str, payload: serde_json::Value, target: Option<String>);
}

pub trait Host: HostSink {
    fn spawn(&self, task: BoxFuture<'static, ()>);          // 默认 tokio::spawn
    fn config_dir(&self) -> PathBuf;                        // agents.json 所在目录
    fn resource_dir(&self) -> Option<PathBuf>;              // bundled adapters
    fn sink_arc(&self) -> Arc<dyn HostSink>;                // 供后台任务持有
    fn spawn_agent_process(&self, spec: AgentProcessSpec)   // 启动策略（容器/ssh 可替换）
        -> Result<AgentProcess, AppError>;
}
```

- `MockSink`（`Arc<Mutex<Vec<CapturedEvent>>>`）记录 `(event, payload, target)`；`MockHost` 组合它并提供临时 `config_dir`。
- `LocalHost`：`config_dir = agentero_core::paths::agentero_config_dir()`、`resource_dir = AGENTERO_RESOURCE_DIR`、默认本地 `exec`。
- `AgentEventEmitter` 保留原签名（`emit(&self, event, payload)`），内部只剩 `sink.emit(event, json!(payload), target)`，因此 **26 个既有 emit 调用点零改动**。
- `AgentHost`（service.rs）是 web 层唯一入口：`new(Arc<dyn Host>)` 后依次提供 32 条命令对应的 `async fn`；后台 turn 通过 `Host::spawn` 执行，事件 target 全程保留。
- `spawn_agent_process` 已定义并在 `MockHost` 单测中验证；当前 ACP 启动仍走 `to_acp_agent` 的本地策略（PATH → bundled → 裸命令）。容器 / `ssh` 覆写属于 B 阶段的启动层接入点。

## 7. 里程碑与剩余工作

| 阶段 | 内容 | 状态 |
|---|---|---|
| **A**（本轮） | `agentero-host` crate + HostSink/MockSink/LocalHost + models + registry（含 lifecycle 安装引擎）+ acp（client/terminal/updates/interaction/ask_user/probe/session）+ runtime + session（run/pool/warm/history/config/handlers）+ prompt/context + doctor/doctor_agents/citation + `AgentHost` 门面 + fake-ACP 集成测试 | ✅ 本轮交付 |
| **B** | `agentero-web`：32 个 RPC handler、`sessionId→label` 路由、`/api/events` 联调、unsupported stub | 约 1–1.5 人日 |
| **C** | 部署与 E2E：镜像内 Node + adapters、服务器上起 claude/codex、浏览器流式对话、进程回收/日志脱敏 | 约 1–1.5 人日 |

**剩余估计：B+C ≈ 2–3 人日**（不含上游漂移 rebase）。最大不确定性已消除：搬运量与接缝数量已量化，ACP 传输无需重写。

## 8. 方案对比：继续移植 vs 新写最小 ACP 客户端

| 维度 | 继续移植（已执行，推荐） | 新写 3–5K 行最小客户端 |
|---|---|---|
| 工作量 | 20.6K LOC 直接搬 + ~200 行接缝改造；已完成 | 协议/流式/取消/权限 3–5K 行；从零补 12 个模板、探测、warm、18 事件，≥ 8–10 人日 |
| 功能对等 | 32 命令 / 18 事件 / warm pool / 权限门控 / 历史回放 | 首版必然缺 warm、历史回放、模型/effort/协作模式、elicitation、ask-user |
| 风险 | 编译面大但机械；单测逐模块搬，已全绿 | 协议边界（ACP 版本、扩展方法、适配器怪癖）需重新踩坑 |
| 上游维护 | 与桌面共用一份代码，修 bug 双向受益 | 双实现漂移 |
| 代码证据 | `acp/client.rs` 已是宿主策略层，传输由 ACP SDK 完成；`session/pool.rs`、`gates.rs`、`updates.rs` 均 0 tauri 依赖 | 重写等于重造 ACP SDK 的 stdio 帧层与 17 个事件的映射 |

**建议：继续移植（已按此执行）。** 唯一的产品降级项是 `remote_agent_*`（SSH 远程 Vault）与桌面终端动作在 v1 不做。

## 9. 验收与测试（本轮实测）

```
$ cargo test -p agentero-host
     Running unittests src/lib.rs
test result: ok. 229 passed; 0 failed; 1 ignored
     Running tests/fake_acp.rs
test result: ok. 7 passed; 0 failed
```

集成测试（`tests/fake_acp.rs` + `tests/fixtures/fake_acp_agent.js`，只用 `node` 标准库）：

1. `prompt_turn_streams_updates_and_completes` — initialize / session/new / prompt 流式：`agent:status` → `agent:stream` → `agent:tool` → `agent:usage` → `agent:completed`，target 全为 `tab-1`。
2. `prompt_turn_cancels_cooperatively` — 300ms 后置 cancel：发出 `session/cancel`，`agent:completed.stopReason = "cancelled"`。
3. `probe_reports_the_fake_agent_identity` — `initialize` 探测返回 agent 名与 session capabilities。
4. `list_and_load_sessions_via_acp` — `session/list` 分页 + `session/load` 回放。
5. `service_run_once_streams_through_the_host_sink` — **真实 `AgentHost::accept_run_once` → `session/run.rs` 全链路**，后台 `Host::spawn`，事件按 target 投递。
6. `registry_persists_under_the_host_config_dir` — `agents.json` 落盘/重载/default。
7. `catalog_exposes_every_builtin_template` — 12 个内置模板。

其他校验：
- `cargo tree -p agentero-host | grep tauri` → **空**（无 tauri/wry/tao）。
- 未修改 `src-tauri/**` 与 `crates/agentero-web/**`；根 `Cargo.toml` 仅新增 members 一项。
- 1 个 ignored 测试需要官方 Dsh npm 包（`AGENTERO_DSH_HISTORY_*`），与移植无关。

## 10. 已交付文件

```
crates/agentero-host/
  Cargo.toml
  src/{lib,host,models,service,doctor,doctor_agents,citation,context_turn,remote_host}.rs
  src/acp/{mod,client,terminal,updates,interaction,ask_user,probe,session}.rs
  src/registry/{mod,store,templates,discovery,bundled,version_check,antigravity,dsh,lifecycle}.rs
  src/runtime/{mod,events,control,gates,stream}.rs
  src/session/{mod,run,pool,warm,handlers,history,config}.rs + session/dsh_history.rs
  src/prompt/{mod,envelope,skills}.rs
  tests/fake_acp.rs
  tests/fixtures/fake_acp_agent.js
docs/development/webui-agent-port.md
```
