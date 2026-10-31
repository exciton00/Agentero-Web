//! Read-only compatibility for Dsh's resume-only ACP bridge. Never migrate,
//! truncate, or otherwise modify the provider's durable session artifacts.
use super::ReplayBuilder;
use crate::features::agent::models::AcpLoadSessionResult;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn home(env: &HashMap<String, String>, cwd: &Path) -> Result<PathBuf, String> {
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let user_home = env
        .get(home_key)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or("Dsh history: cannot locate the user home")?;
    let root = match env.get("DSH_HOME").filter(|s| !s.trim().is_empty()) {
        Some(value) if value == "~" => user_home,
        Some(value) if value.starts_with("~/") || value.starts_with("~\\") => {
            user_home.join(&value[2..])
        }
        Some(value) => PathBuf::from(value),
        None => user_home.join(".dsh"),
    };
    Ok(if root.is_absolute() {
        root
    } else {
        cwd.join(root)
    })
}

/// ACP-created sessions use a bare UUID; sessions created by the Dsh CLI/Web
/// use the same UUID with a `session-` prefix. The log directory is named after
/// the session header id in both layouts.
fn valid_session_id(sid: &str) -> bool {
    let uuid = sid.strip_prefix("session-").unwrap_or(sid);
    uuid.len() == 36 && uuid::Uuid::parse_str(uuid).is_ok()
}

/// Both directory/header spellings a session id may use on disk.
fn session_id_candidates(sid: &str) -> [String; 2] {
    match sid.strip_prefix("session-") {
        Some(bare) => [sid.to_string(), bare.to_string()],
        None => [sid.to_string(), format!("session-{sid}")],
    }
}

/// Highest Session format generation this reader understands. A newer Dsh may
/// bump the generation while keeping the append-only event vocabulary; unknown
/// generations above this bound are refused rather than guessed.
const MAX_SUPPORTED_FORMAT: u64 = 6;

/// Canonical storage roots a Dsh home may use for session logs.
fn storage_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("sessions"),
        home.join("storages").join("sessions"),
    ]
}

/// Session log directories for one id spelling: the released
/// sessions/<project>/<id> layout and a flat sessions/<id> layout.
fn session_log_dirs(root: &Path, candidate: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(projects) = fs::read_dir(root) {
        for project in projects.flatten() {
            if project.file_type().is_ok_and(|kind| kind.is_dir()) {
                let dir = project.path().join(candidate);
                if dir.is_dir() {
                    dirs.push(dir);
                }
            }
        }
    }
    let flat = root.join(candidate);
    if flat.is_dir() && !dirs.iter().any(|dir| dir == &flat) {
        dirs.push(flat);
    }
    dirs
}

/// The provider's released layout is sessions/<project>/<id>/session[.vN].jsonl[.zstd].
/// Project level and id spelling are checked against the log header. A flat or
/// storages/sessions fallback covers newer persistence roots.
pub(super) fn load(home: &Path, sid: &str, cwd: &Path) -> Result<AcpLoadSessionResult, String> {
    // A session id is a single path component even on Windows (no alternate
    // data streams); only the two known UUID spellings are accepted.
    if !valid_session_id(sid) {
        return Err("Dsh history: invalid session id".into());
    }
    let candidates = session_id_candidates(sid);
    let roots = storage_roots(home);
    let mut failures: Vec<String> = Vec::new();
    for root in &roots {
        if !root.is_dir() {
            continue;
        }
        for candidate in &candidates {
            for dir in session_log_dirs(root, candidate) {
                let mut logs = Vec::new();
                for entry in fs::read_dir(&dir).map_err(|e| format!("Dsh history: {e}"))? {
                    let entry = entry.map_err(|e| format!("Dsh history: {e}"))?;
                    if let Some(version) = log_version(&entry.file_name().to_string_lossy()) {
                        logs.push((version, entry.path()));
                    }
                }
                logs.sort_by_key(|(version, _)| *version);
                let Some((version, path)) = logs.pop() else {
                    failures.push(format!("no committed log in {}", dir.display()));
                    continue;
                };
                if !(2..=MAX_SUPPORTED_FORMAT).contains(&version)
                    || logs.last().is_some_and(|(other, _)| *other == version)
                {
                    failures.push(format!(
                        "unsupported or ambiguous format v{version} at {}",
                        path.display()
                    ));
                    continue;
                }
                match read_log(&path, version, sid, candidate, cwd) {
                    Ok(history) => return Ok(history),
                    Err(error) => failures.push(error),
                }
            }
        }
    }
    if failures.is_empty() {
        Err(format!(
            "Dsh history: session log not found under {}; check the agent's DSH_HOME and custom persistence configuration",
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ")
        ))
    } else {
        Err(format!(
            "Dsh history: cannot restore session: {}",
            failures.join("; ")
        ))
    }
}

fn log_version(name: &str) -> Option<u64> {
    let name = name
        .strip_suffix(".zstd")
        .or_else(|| name.strip_suffix(".zst"))
        .unwrap_or(name);
    if name == "session.jsonl" {
        return Some(0);
    }
    let number = name.strip_prefix("session.v")?.strip_suffix(".jsonl")?;
    let version = number.parse::<u64>().ok()?;
    (version > 0 && number == version.to_string()).then_some(version)
}

fn same_cwd(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        crate::core::process::windows_shell_path(path)
            .to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .to_string()
    };
    let left = normalize(left);
    let right = normalize(right);
    if cfg!(windows) {
        left.replace('/', "\\")
            .eq_ignore_ascii_case(&right.replace('/', "\\"))
    } else {
        left == right
    }
}

/// Content blocks of one settlement message; tolerates the known shapes.
fn message_blocks(message: &Value) -> Vec<Value> {
    let content = &message["content"];
    if let Some(blocks) = content.as_array() {
        return blocks.clone();
    }
    if let Some(blocks) = content["blocks"].as_array() {
        return blocks.clone();
    }
    if let Some(text) = content.as_str() {
        return vec![serde_json::json!({"type": "text", "text": text})];
    }
    if let Some(text) = message["text"].as_str() {
        return vec![serde_json::json!({"type": "text", "text": text})];
    }
    Vec::new()
}

/// The stream embedded in a settlement event, tolerating nested shapes.
fn event_stream(data: &Value) -> Option<&Value> {
    [
        &data["stream"],
        &data["message"]["stream"],
        &data["attempt"]["stream"],
    ]
    .into_iter()
    .find(|candidate| candidate.is_array())
}

fn blocks_have_visible_text(blocks: &[Value]) -> bool {
    blocks.iter().any(|block| {
        block["type"] == "text"
            && block["text"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
    })
}

/// Visible text blocks of one settlement stream, in order.
fn stream_text_blocks(stream: &Value) -> Vec<Value> {
    stream_blocks(stream)
        .into_iter()
        .filter(|block| {
            block["type"] == "text"
                && block["text"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty())
        })
        .collect()
}

/// How the settlement events of one log map onto transcript lines.
#[derive(Default)]
struct SettlementPlan {
    /// Steps settled by an `assistant/message` (attempt events are not replayed).
    message_steps: HashSet<(u64, u64)>,
    /// First `assistant/message` index per step (where merged text is applied).
    message_merge_index: HashMap<(u64, u64), usize>,
    /// Visible text blocks recovered from an attempt for a textless message.
    message_merge_text: HashMap<(u64, u64), Vec<Value>>,
    /// Best attempt settlement per step that has no `assistant/message`.
    attempt_choice: HashMap<(u64, u64), usize>,
}

fn plan_settlements(events: &[Value]) -> SettlementPlan {
    let mut plan = SettlementPlan::default();
    let mut textless_steps = HashSet::new();
    for (index, event) in events.iter().enumerate() {
        if event["type"] != "assistant/message" {
            continue;
        }
        let Some(step) = step_key(event) else {
            continue;
        };
        plan.message_steps.insert(step);
        plan.message_merge_index.entry(step).or_insert(index);
        let data = &event["data"];
        let message = data.get("message").unwrap_or(data);
        if !blocks_have_visible_text(&message_blocks(message)) {
            textless_steps.insert(step);
        }
    }
    // Retries may settle the same step several times; prefer the attempt that
    // streamed the most visible text.
    let mut best: HashMap<(u64, u64), (usize, usize)> = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        if event["type"] != "assistant/attempt" {
            continue;
        }
        let Some(step) = step_key(event) else {
            continue;
        };
        if plan.message_steps.contains(&step) {
            continue;
        }
        let len = event_stream(&event["data"])
            .map(|stream| {
                stream_text_blocks(stream)
                    .iter()
                    .filter_map(|block| block["text"].as_str().map(str::len))
                    .sum::<usize>()
            })
            .unwrap_or(0);
        match best.get(&step) {
            Some((_, best_len)) if *best_len >= len => {}
            _ => {
                best.insert(step, (index, len));
            }
        }
    }
    plan.attempt_choice = best
        .into_iter()
        .map(|(step, (index, _))| (step, index))
        .collect();
    // A completed message with reasoning/tools but no visible text may still
    // have settled an attempt for the same step whose stream holds the answer.
    let mut attempt_text: HashMap<(u64, u64), (usize, usize)> = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        if event["type"] != "assistant/attempt" {
            continue;
        }
        let Some(step) = step_key(event) else {
            continue;
        };
        if !textless_steps.contains(&step) {
            continue;
        }
        let blocks = event_stream(&event["data"])
            .map(stream_text_blocks)
            .unwrap_or_default();
        let len = blocks
            .iter()
            .filter_map(|block| block["text"].as_str().map(str::len))
            .sum::<usize>();
        match attempt_text.get(&step) {
            Some((_, best_len)) if *best_len >= len => {}
            _ => {
                attempt_text.insert(step, (index, len));
            }
        }
    }
    for (step, (index, len)) in attempt_text {
        if len == 0 {
            continue;
        }
        if let Some(stream) = event_stream(&events[index]["data"]) {
            plan.message_merge_text
                .insert(step, stream_text_blocks(stream));
        }
    }
    plan
}

fn read_log(
    path: &Path,
    version: u64,
    requested_sid: &str,
    header_sid: &str,
    cwd: &Path,
) -> Result<AcpLoadSessionResult, String> {
    let file = File::open(path).map_err(|e| format!("Dsh history: {e}"))?;
    let reader: Box<dyn Read> = if path
        .extension()
        .is_some_and(|ext| ext == "zstd" || ext == "zst")
    {
        Box::new(zstd::stream::read::Decoder::new(file).map_err(|e| format!("Dsh history: {e}"))?)
    } else {
        Box::new(file)
    };
    let mut bytes = Vec::new();
    reader
        .take(MAX_LOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Dsh history: cannot decode session log: {e}"))?;
    if bytes.len() as u64 > MAX_LOG_BYTES {
        return Err("Dsh history: session log exceeds the 64 MiB replay limit".into());
    }
    let raw = std::str::from_utf8(&bytes).map_err(|e| format!("Dsh history: {e}"))?;
    let mut rows = raw.lines();
    let header: Value = serde_json::from_str(rows.next().unwrap_or_default())
        .map_err(|e| format!("Dsh history: invalid session header: {e}"))?;
    let header_version = header["version"].as_u64().or_else(|| {
        header["version"]
            .as_str()
            .and_then(|value| value.parse().ok())
    });
    let header_cwd_matches = header["cwd"]
        .as_str()
        .is_some_and(|path| same_cwd(Path::new(path), cwd));
    if header["type"] != "session"
        || header["id"] != header_sid
        || header_version != Some(version)
        || !header_cwd_matches
        || header["origin"] == "subagent"
        || header["parentSession"].is_string()
    {
        return Err(format!(
            "Dsh history: session header mismatch path={} expected_id={header_sid} header_id={:?} expected_version={version} header_version={header_version:?} cwd_matches={header_cwd_matches} origin={:?} parent={:?}",
            path.display(),
            header["id"].as_str().unwrap_or(""),
            header["origin"].as_str().unwrap_or(""),
            header["parentSession"].as_str().unwrap_or(""),
        ));
    }
    let mut events = Vec::new();
    for (seq, row) in rows.enumerate() {
        let event: Value = serde_json::from_str(row)
            .map_err(|e| format!("Dsh history: invalid session event {seq}: {e}"))?;
        if event["seq"].as_u64() != Some(seq as u64) || !event["type"].is_string() {
            return Err("Dsh history: unsupported or non-contiguous session events".into());
        }
        events.push(event);
    }
    let plan = plan_settlements(&events);
    let mut replay = ReplayBuilder::default();
    for (index, event) in events.iter().enumerate() {
        match event["type"].as_str() {
            Some("assistant/message") => {
                apply_message(&mut replay, event);
                if let Some(step) = step_key(event) {
                    if plan.message_merge_index.get(&step) == Some(&index) {
                        if let Some(blocks) = plan.message_merge_text.get(&step) {
                            for block in blocks {
                                if let Some(text) = block["text"].as_str() {
                                    replay.push_agent_chunk(
                                        false,
                                        text.to_string(),
                                        Some(format!("dsh-attempt-text-{}", event["seq"])),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Some("assistant/attempt") => {
                let Some(step) = step_key(event) else {
                    continue;
                };
                if plan.message_steps.contains(&step) {
                    continue;
                }
                if plan.attempt_choice.get(&step) != Some(&index) {
                    continue;
                }
                if let Some(stream) = event_stream(&event["data"]) {
                    apply_stream(
                        &mut replay,
                        stream,
                        Some(format!("dsh-attempt-{}", event["seq"])),
                    );
                }
            }
            _ => apply_event(&mut replay, event),
        }
    }
    let recovered_events = recover_unknown_settlement_text(&mut replay, &events);
    let (lines, title) = replay.finish();
    let agent_text_chars: usize = lines
        .iter()
        .filter(|line| line.kind == "agent")
        .map(|line| line.text.chars().count())
        .sum();
    let mut event_types: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for event in &events {
        if let Some(event_type) = event["type"].as_str() {
            *event_types.entry(event_type).or_default() += 1;
        }
    }
    log::info!(
        "dsh history: restored path={} version={} events={} lines={} agent_text_chars={} merged_steps={} attempt_steps={} recovered_events={} event_types={}",
        path.display(),
        version,
        events.len(),
        lines.len(),
        agent_text_chars,
        plan.message_merge_text.len(),
        plan.attempt_choice.len(),
        recovered_events.join(","),
        event_types
            .iter()
            .map(|(event_type, count)| format!("{event_type}:{count}"))
            .collect::<Vec<_>>()
            .join(","),
    );
    Ok(AcpLoadSessionResult {
        session_id: requested_sid.to_string(),
        title,
        lines,
    })
}

/// The `(turn, step)` a model settlement belongs to.
fn step_key(event: &Value) -> Option<(u64, u64)> {
    let data = &event["data"];
    Some((data["turn"].as_u64()?, data["step"].as_u64()?))
}

/// Apply one completed `assistant/message`, recovering text that only reached
/// the embedded stream when the content blocks carry none.
fn apply_message(replay: &mut ReplayBuilder, event: &Value) {
    let data = &event["data"];
    let message = data.get("message").unwrap_or(data);
    let id = message["id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| Some(format!("dsh-{}", event["seq"])));
    let blocks = message_blocks(message);
    if blocks.is_empty() {
        if let Some(stream) = event_stream(data) {
            apply_stream(replay, stream, id);
        }
        return;
    }
    for block in &blocks {
        if matches!(block["type"].as_str(), Some("text" | "reasoning")) {
            if let Some(text) = block["text"].as_str() {
                replay.push_agent_chunk(block["type"] == "reasoning", text.to_string(), id.clone());
            }
        }
    }
    if !blocks_have_visible_text(&blocks) {
        if let Some(stream) = event_stream(data) {
            for block in stream_text_blocks(stream) {
                if let Some(text) = block["text"].as_str() {
                    replay.push_agent_chunk(
                        false,
                        text.to_string(),
                        Some(format!("dsh-msg-stream-{}", event["seq"])),
                    );
                }
            }
        }
    }
}

fn apply_event(replay: &mut ReplayBuilder, event: &Value) {
    let data = &event["data"];
    let message = data.get("message").unwrap_or(data);
    let id = message["id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| Some(format!("dsh-{}", event["seq"])));
    match event["type"].as_str() {
        Some("user/message") if message["source"]["kind"] == "user" => {
            for block in message_blocks(message) {
                if block["type"] == "text" {
                    if let Some(text) = block["text"].as_str() {
                        replay.push_user_chunk(text.to_string(), id.clone());
                    }
                }
            }
        }
        Some("tool/call") => {
            if let (Some(id), Some(name)) = (data["callId"].as_str(), data["name"].as_str()) {
                let input = data["arguments"]
                    .as_str()
                    .map(|s| serde_json::from_str(s).unwrap_or(Value::String(s.to_string())))
                    .or_else(|| data.get("arguments").cloned());
                replay.apply_tool(
                    id.to_string(),
                    Some(name.to_string()),
                    Some("other".into()),
                    Some("in_progress".into()),
                    input,
                    None,
                );
            }
        }
        Some("tool/result") => {
            if let Some(id) = message["toolCallId"].as_str() {
                replay.apply_tool(
                    id.to_string(),
                    None,
                    None,
                    Some(
                        if message["isError"] == true {
                            "failed"
                        } else {
                            "completed"
                        }
                        .into(),
                    ),
                    None,
                    message.get("content").cloned(),
                );
            }
        }
        // `source.kind == "fallback"` is Dsh's auto-truncation of the first
        // prompt line — usually Agentero's own system envelope, never a human
        // label. Keep provider/user titles and let the UI derive a title from
        // the first user message when only a fallback exists.
        Some("session/title") if data["source"]["kind"] != "fallback" => {
            replay.title = data["title"].as_str().map(str::to_owned);
        }
        _ => {}
    }
}

/// Text candidates carried by a settlement event generation this reader does
/// not know yet (e.g. a renamed assistant settlement in a newer Dsh).
fn generic_text_candidates(event: &Value) -> Vec<String> {
    let data = &event["data"];
    let mut out = Vec::new();
    for source in [&data["message"], &data["response"], data] {
        for block in message_blocks(source) {
            if block["type"] == "text" {
                if let Some(text) = block["text"].as_str() {
                    out.push(text.to_string());
                }
            }
        }
    }
    if let Some(stream) = event_stream(data) {
        for block in stream_text_blocks(stream) {
            if let Some(text) = block["text"].as_str() {
                out.push(text.to_string());
            }
        }
    }
    if let Some(text) = data["text"].as_str() {
        out.push(text.to_string());
    }
    out
}

/// Last resort when the transcript saved no visible answer: recover text from
/// unknown `assistant/*` settlement events (newer Dsh generations) without
/// duplicating content already replayed. Returns the recovered event types.
fn recover_unknown_settlement_text(replay: &mut ReplayBuilder, events: &[Value]) -> Vec<String> {
    if !replay.agent_answer_text().trim().is_empty() {
        return Vec::new();
    }
    let mut recovered = Vec::new();
    let mut seen = replay.agent_answer_text();
    for event in events {
        let Some(event_type) = event["type"].as_str() else {
            continue;
        };
        if matches!(event_type, "assistant/message" | "assistant/attempt")
            || !event_type.starts_with("assistant/")
        {
            continue;
        }
        for text in generic_text_candidates(event) {
            let text = text.trim();
            if text.is_empty() || seen.contains(text) {
                continue;
            }
            replay.push_agent_chunk(
                false,
                text.to_string(),
                Some(format!("dsh-recovered-{}", event["seq"])),
            );
            seen.push_str(text);
            if !recovered.iter().any(|name| name == event_type) {
                recovered.push(event_type.to_string());
            }
        }
    }
    recovered
}

/// Apply the content blocks reconstructed from one settlement stream.
fn apply_stream(replay: &mut ReplayBuilder, stream: &Value, id: Option<String>) {
    for block in stream_blocks(stream) {
        match block["type"].as_str() {
            Some("text") => {
                if let Some(text) = block["text"].as_str() {
                    replay.push_agent_chunk(false, text.to_string(), id.clone());
                }
            }
            Some("reasoning") => {
                if let Some(text) = block["text"].as_str() {
                    replay.push_agent_chunk(true, text.to_string(), id.clone());
                }
            }
            Some("tool-call") => {
                let Some(call_id) = block["id"].as_str() else {
                    continue;
                };
                let name = block["name"].as_str().map(str::to_owned);
                let input = block["arguments"]
                    .as_str()
                    .map(|raw| serde_json::from_str(raw).unwrap_or(Value::String(raw.to_string())))
                    .or_else(|| block.get("arguments").cloned());
                replay.apply_tool(
                    call_id.to_string(),
                    name,
                    Some("other".into()),
                    Some("in_progress".into()),
                    input,
                    None,
                );
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct StreamPartial {
    block_type: String,
    text: String,
    tool_id: Option<String>,
    tool_name: Option<String>,
    arguments: String,
    block: Option<Value>,
}

/// Rebuild assistant content blocks from a settlement's compact stream.
///
/// Durable settlements store either raw `chunk` records (block start/end and
/// un-packed deltas) or packed delta runs (`text-chunks`, `reasoning-chunks`,
/// `tool-call-chunks`). `block-end` carries the authoritative block; otherwise
/// the accumulated deltas form the visible prefix of an interrupted block.
#[derive(Default)]
struct StreamAssembler {
    order: Vec<u64>,
    partials: HashMap<u64, StreamPartial>,
}

impl StreamAssembler {
    fn ensure(&mut self, index: u64, block_type: &str) -> &mut StreamPartial {
        if !self.partials.contains_key(&index) {
            self.order.push(index);
            self.partials.insert(
                index,
                StreamPartial {
                    block_type: block_type.to_string(),
                    ..StreamPartial::default()
                },
            );
        }
        self.partials.get_mut(&index).expect("just inserted")
    }

    fn push(&mut self, record: &Value) {
        match record["type"].as_str() {
            Some("chunk") => {
                let chunk = &record["chunk"];
                match chunk["type"].as_str() {
                    Some("block-start") => {
                        if let Some(index) = chunk["index"].as_u64() {
                            self.ensure(index, chunk["blockType"].as_str().unwrap_or("text"));
                        }
                    }
                    Some("text-delta") | Some("reasoning-delta") => {
                        let (Some(index), Some(text)) =
                            (chunk["index"].as_u64(), chunk["text"].as_str())
                        else {
                            return;
                        };
                        let kind = if chunk["type"] == "reasoning-delta" {
                            "reasoning"
                        } else {
                            "text"
                        };
                        let partial = self.ensure(index, kind);
                        if partial.block.is_none() {
                            partial.text.push_str(text);
                        }
                    }
                    Some("tool-call-delta") => {
                        let Some(index) = chunk["index"].as_u64() else {
                            return;
                        };
                        let partial = self.ensure(index, "tool-call");
                        if partial.block.is_some() {
                            return;
                        }
                        if let Some(call_id) = chunk["id"].as_str() {
                            partial.tool_id = Some(call_id.to_string());
                        }
                        if let Some(name) = chunk["name"].as_str() {
                            partial.tool_name = Some(name.to_string());
                        }
                        if let Some(delta) = chunk["argumentsDelta"].as_str() {
                            partial.arguments.push_str(delta);
                        }
                    }
                    Some("block-end") => {
                        let Some(index) = chunk["index"].as_u64() else {
                            return;
                        };
                        let block = chunk["block"].clone();
                        let block_type = block["type"].as_str().unwrap_or("text").to_string();
                        let partial = self.ensure(index, &block_type);
                        if partial.block.is_none() {
                            partial.block = Some(block);
                        }
                    }
                    _ => {}
                }
            }
            Some("text-chunks") | Some("reasoning-chunks") => {
                let Some(index) = record["index"].as_u64() else {
                    return;
                };
                let kind = if record["type"] == "text-chunks" {
                    "text"
                } else {
                    "reasoning"
                };
                let text: String = record["texts"]
                    .as_array()
                    .map(|texts| texts.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let partial = self.ensure(index, kind);
                if partial.block.is_none() {
                    partial.text.push_str(&text);
                }
            }
            Some("tool-call-chunks") => {
                let Some(index) = record["index"].as_u64() else {
                    return;
                };
                let partial = self.ensure(index, "tool-call");
                if partial.block.is_some() {
                    return;
                }
                if let Some(call_id) = record["id"].as_str() {
                    partial.tool_id = Some(call_id.to_string());
                }
                if let Some(name) = record["name"].as_str() {
                    partial.tool_name = Some(name.to_string());
                }
                let args: String = record["args"]
                    .as_array()
                    .map(|args| args.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                partial.arguments.push_str(&args);
            }
            _ => {}
        }
    }

    fn blocks(mut self) -> Vec<Value> {
        let mut out = Vec::new();
        for index in std::mem::take(&mut self.order) {
            let Some(partial) = self.partials.remove(&index) else {
                continue;
            };
            if let Some(block) = partial.block {
                match block["type"].as_str() {
                    Some("text" | "reasoning") => {
                        let visible = block["text"]
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty());
                        if visible {
                            out.push(block);
                        }
                    }
                    Some("tool-call") => out.push(block),
                    _ => {}
                }
                continue;
            }
            match partial.block_type.as_str() {
                "text" | "reasoning" => {
                    if !partial.text.trim().is_empty() {
                        let kind = partial.block_type;
                        out.push(serde_json::json!({"type": kind, "text": partial.text}));
                    }
                }
                "tool-call" => {
                    let name = partial.tool_name.unwrap_or_default();
                    if name.is_empty() && partial.arguments.is_empty() {
                        continue;
                    }
                    out.push(serde_json::json!({
                        "type": "tool-call",
                        "id": partial.tool_id.unwrap_or_else(|| format!("dsh-call-{index}")),
                        "name": name,
                        "arguments": partial.arguments,
                    }));
                }
                _ => {}
            }
        }
        out
    }
}

fn stream_blocks(stream: &Value) -> Vec<Value> {
    let mut assembler = StreamAssembler::default();
    if let Some(records) = stream.as_array() {
        for record in records {
            assembler.push(record);
        }
    }
    assembler.blocks()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SID: &str = "455181d7-12ab-4567-8901-0123456789ab";
    const PREFIXED_SID: &str = "session-455181d7-12ab-4567-8901-0123456789ab";

    fn write_log(dir: &Path, id: &str, version: u64, compressed: bool, title: Value) -> PathBuf {
        let folder = dir.join("sessions/project").join(id);
        fs::create_dir_all(&folder).unwrap();
        let events = [
            json!({"type":"session","version":version,"id":id,"cwd":dir.to_string_lossy()}),
            json!({"seq":0,"type":"user/message","data":{"id":"u1","source":{"kind":"user"},"content":[{"type":"text","text":"first question"}]}}),
            json!({"seq":1,"type":"user/message","data":{"id":"ctx","source":{"kind":"plugin"},"content":[{"type":"text","text":"hidden context"}]}}),
            json!({"seq":2,"type":"assistant/message","data":{"message":{"id":"a1","content":[{"type":"reasoning","text":"thinking"},{"type":"text","text":"first answer"}]}}}),
            json!({"seq":3,"type":"tool/call","data":{"callId":"t1","name":"read","arguments":"{\"path\":\"note.md\"}"}}),
            json!({"seq":4,"type":"tool/result","data":{"message":{"toolCallId":"t1","content":[{"type":"text","text":"note"}]}}}),
            json!({"seq":5,"type":"user/message","data":{"id":"u2","source":{"kind":"user"},"content":[{"type":"text","text":"second question"}]}}),
            json!({"seq":6,"type":"assistant/message","data":{"message":{"id":"a2","content":[{"type":"text","text":"second answer"}]}}}),
            json!({"seq":7,"type":"session/title","data": title}),
        ];
        let raw = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let name = if version == 0 {
            "session.jsonl".into()
        } else {
            format!("session.v{version}.jsonl")
        };
        let path = folder.join(if compressed {
            format!("{name}.zstd")
        } else {
            name
        });
        let bytes = if compressed {
            zstd::stream::encode_all(raw.as_bytes(), 0).unwrap()
        } else {
            raw.into_bytes()
        };
        fs::write(&path, bytes).unwrap();
        path
    }

    fn fixture(dir: &Path, version: u64, compressed: bool) -> PathBuf {
        write_log(
            dir,
            SID,
            version,
            compressed,
            json!({"title":"my conversation"}),
        )
    }

    #[test]
    fn restores_compressed_multi_turn_history_and_uses_latest_generation() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), 0, false);
        fixture(dir.path(), 4, true);
        let history = load(dir.path(), SID, dir.path()).unwrap();
        assert_eq!(history.title.as_deref(), Some("my conversation"));
        assert_eq!(
            history
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            [
                "first question",
                "first answer",
                "second question",
                "second answer"
            ]
        );
        assert_eq!(history.lines[1].reasoning.as_deref(), Some("thinking"));
        assert!(history.lines[1].parts.iter().any(|part| matches!(part,
            crate::features::agent::models::AcpHistoryPart::Tool { tool } if tool.status == "completed")));
    }

    fn write_events(dir: &Path, id: &str, version: u64, events: Vec<Value>) -> PathBuf {
        let folder = dir.join("sessions/project").join(id);
        fs::create_dir_all(&folder).unwrap();
        let header =
            json!({"type":"session","version":version,"id":id,"cwd":dir.to_string_lossy()});
        let mut raw = String::new();
        raw.push_str(&header.to_string());
        raw.push('\n');
        for (seq, event) in events.iter().enumerate() {
            let mut event = event.clone();
            if let Some(object) = event.as_object_mut() {
                object.insert("seq".to_string(), json!(seq));
            }
            raw.push_str(&event.to_string());
            raw.push('\n');
        }
        let path = folder.join(format!("session.v{version}.jsonl"));
        fs::write(&path, raw.into_bytes()).unwrap();
        path
    }

    #[test]
    fn restores_answers_from_failed_attempt_streams() {
        let dir = tempfile::tempdir().unwrap();
        write_events(
            dir.path(),
            SID,
            4,
            vec![
                json!({"type":"user/message","data":{"id":"u1","source":{"kind":"user"},"content":[{"type":"text","text":"question"}]}}),
                json!({"type":"assistant/attempt","data":{"turn":1,"step":1,"stream":[
                    {"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"reasoning"}},
                    {"type":"reasoning-chunks","index":0,"texts":["think"],"dt":[]},
                    {"type":"chunk","chunk":{"type":"block-start","index":1,"blockType":"text"}},
                    {"type":"text-chunks","index":1,"texts":["最终","回答"],"dt":[]},
                    {"type":"chunk","chunk":{"type":"block-end","index":1,"block":{"type":"text","text":"最终回答"}}},
                    {"type":"chunk","chunk":{"type":"finish","reason":{"kind":"error","failure":{"message":"boom"}}}}
                ]}}),
            ],
        );
        let history = load(dir.path(), SID, dir.path()).unwrap();
        assert_eq!(history.lines.len(), 2);
        assert_eq!(history.lines[1].kind, "agent");
        assert_eq!(history.lines[1].text, "最终回答");
        assert_eq!(history.lines[1].reasoning.as_deref(), Some("think"));
    }

    #[test]
    fn keeps_partial_open_blocks_and_prefers_settled_visible_text() {
        let dir = tempfile::tempdir().unwrap();
        write_events(
            dir.path(),
            SID,
            4,
            vec![
                json!({"type":"user/message","data":{"id":"u1","source":{"kind":"user"},"content":[{"type":"text","text":"question"}]}}),
                // Step 1 aborts before its block closes: keep the streamed prefix.
                json!({"type":"assistant/attempt","data":{"turn":1,"step":1,"stream":[
                    {"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"text"}},
                    {"type":"text-chunks","index":0,"texts":["部分"],"dt":[]}
                ]}}),
                // Step 2 settles twice; the retry with the longer visible text wins.
                json!({"type":"assistant/attempt","data":{"turn":1,"step":2,"stream":[
                    {"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"text"}},
                    {"type":"text-chunks","index":0,"texts":["短的"],"dt":[]}
                ]}}),
                json!({"type":"assistant/attempt","data":{"turn":1,"step":2,"stream":[
                    {"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"text"}},
                    {"type":"text-chunks","index":0,"texts":["更长的重试回答"],"dt":[]}
                ]}}),
                // A completed step wins over any attempt settled for it.
                json!({"type":"assistant/attempt","data":{"turn":2,"step":1,"stream":[
                    {"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"text"}},
                    {"type":"text-chunks","index":0,"texts":["失败尝试"],"dt":[]}
                ]}}),
                json!({"type":"assistant/message","data":{"turn":2,"step":1,"message":{"id":"m1","content":[{"type":"text","text":"真正回答"}]}}}),
            ],
        );
        let history = load(dir.path(), SID, dir.path()).unwrap();
        let text: String = history
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert!(text.contains("部分"), "{text}");
        assert!(text.contains("更长的重试回答"), "{text}");
        assert!(!text.contains("短的"), "{text}");
        assert!(!text.contains("失败尝试"), "{text}");
        assert!(text.contains("真正回答"), "{text}");
    }

    #[test]
    fn matches_both_session_id_spellings_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_log(
            dir.path(),
            PREFIXED_SID,
            4,
            true,
            json!({"title":"prefixed title"}),
        );
        // A bare-ACP id addresses a log directory/header using the prefixed form.
        let history = load(dir.path(), SID, dir.path()).unwrap();
        assert_eq!(history.session_id, SID);
        assert_eq!(history.title.as_deref(), Some("prefixed title"));
        assert_eq!(history.lines[0].text, "first question");
        // The prefixed request still resolves the same directory.
        let history = load(dir.path(), PREFIXED_SID, dir.path()).unwrap();
        assert_eq!(history.session_id, PREFIXED_SID);

        // And the reverse: a prefixed request over a bare directory/header.
        fs::remove_dir_all(dir.path().join("sessions")).unwrap();
        write_log(dir.path(), SID, 4, true, json!({"title":"bare title"}));
        let history = load(dir.path(), PREFIXED_SID, dir.path()).unwrap();
        assert_eq!(history.session_id, PREFIXED_SID);
        assert_eq!(history.title.as_deref(), Some("bare title"));

        assert!(!valid_session_id("455181d7-12ab-4567-8901-0123456789ab/x"));
        assert!(!valid_session_id("session-455181d7"));
    }

    #[test]
    fn recovers_text_from_unknown_assistant_settlements() {
        let dir = tempfile::tempdir().unwrap();
        write_events(
            dir.path(),
            SID,
            5,
            vec![
                json!({"type":"user/message","data":{"id":"u1","source":{"kind":"user"},"content":[{"type":"text","text":"question"}]}}),
                json!({"type":"assistant/message","data":{"turn":1,"step":1,"message":{"id":"m1","content":[{"type":"reasoning","text":"think"}]}}}),
                // A newer settlement generation this reader does not know yet.
                json!({"type":"assistant/response","data":{"turn":1,"step":2,"message":{"role":"assistant","content":[{"type":"text","text":"新格式回答"}]}}}),
                json!({"type":"turn/end","data":{"turn":1,"reason":{"kind":"completed"}}}),
            ],
        );
        let history = load(dir.path(), SID, dir.path()).unwrap();
        let text: String = history
            .lines
            .iter()
            .filter(|line| line.kind == "agent")
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(text, "新格式回答");
    }

    #[test]
    fn accepts_v5_and_v6_generations_and_refuses_newer_ones() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), 5, true);
        let history = load(dir.path(), SID, dir.path()).unwrap();
        assert_eq!(history.lines[1].text, "first answer");
        fs::remove_dir_all(dir.path().join("sessions")).unwrap();
        fixture(dir.path(), 6, false);
        assert!(load(dir.path(), SID, dir.path()).is_ok());
        fs::remove_dir_all(dir.path().join("sessions")).unwrap();
        fixture(dir.path(), 7, false);
        assert!(load(dir.path(), SID, dir.path())
            .unwrap_err()
            .contains("unsupported"));
    }

    #[test]
    fn merges_attempt_text_into_a_textless_completed_message() {
        let dir = tempfile::tempdir().unwrap();
        let session_stream = json!([
            {"type":"chunk","chunk":{"type":"block-start","index":0,"blockType":"reasoning"}},
            {"type":"reasoning-chunks","index":0,"texts":["think"],"dt":[]},
            {"type":"chunk","chunk":{"type":"block-start","index":1,"blockType":"text"}},
            {"type":"text-chunks","index":1,"texts":["最终","回答"],"dt":[]},
            {"type":"chunk","chunk":{"type":"block-end","index":1,"block":{"type":"text","text":"最终回答"}}}
        ]);
        write_events(
            dir.path(),
            SID,
            4,
            vec![
                json!({"type":"user/message","data":{"id":"u1","source":{"kind":"user"},"content":[{"type":"text","text":"question"}]}}),
                // The completed settlement carries only reasoning/tools ...
                json!({"type":"assistant/message","data":{"turn":1,"step":1,"message":{"id":"m1","content":[
                    {"type":"reasoning","text":"think"},
                    {"type":"tool-call","id":"t1","name":"read","arguments":"{}"}
                ]}}}),
                // ... while the answer streamed in the settled attempt.
                json!({"type":"assistant/attempt","data":{"turn":1,"step":1,"stream":session_stream}}),
                json!({"type":"tool/call","data":{"callId":"t1","name":"read","arguments":"{}"}}),
                json!({"type":"turn/end","data":{"turn":1,"reason":{"kind":"completed"}}}),
            ],
        );
        let history = load(dir.path(), SID, dir.path()).unwrap();
        assert_eq!(history.lines[1].kind, "agent");
        assert_eq!(history.lines[1].text, "最终回答");
        // Reasoning is not duplicated by the merge, and the tool survives.
        assert_eq!(history.lines[1].reasoning.as_deref(), Some("think"));
        assert!(history.lines[1].parts.iter().any(|part| matches!(
            part,
            crate::features::agent::models::AcpHistoryPart::Tool { tool } if tool.id == "t1"
        )));
    }

    #[test]
    fn ignores_dsh_fallback_titles_but_keeps_human_titles() {
        let dir = tempfile::tempdir().unwrap();
        write_log(
            dir.path(),
            SID,
            4,
            false,
            json!({
                "title":"You are an assistant working",
                "messageSeqs":[0],
                "source":{"kind":"fallback"}
            }),
        );
        // A fallback title is Dsh's truncated first prompt line, not a label;
        // the UI derives one from the first user message instead.
        assert_eq!(load(dir.path(), SID, dir.path()).unwrap().title, None);
        fs::remove_dir_all(dir.path().join("sessions")).unwrap();
        write_log(
            dir.path(),
            SID,
            4,
            false,
            json!({
                "title":"GaN trap analysis",
                "source":{"kind":"provider","provider":"session-title-first-prompt-llm"}
            }),
        );
        assert_eq!(
            load(dir.path(), SID, dir.path()).unwrap().title.as_deref(),
            Some("GaN trap analysis")
        );
    }

    #[test]
    fn refuses_wrong_workspace_unsafe_ids_and_newer_or_corrupt_logs() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path(), 4, false);
        assert!(load(dir.path(), SID, &dir.path().join("other")).is_err());
        assert!(load(dir.path(), "../outside", dir.path()).is_err());
        assert!(load(dir.path(), "455181d7:stream", dir.path()).is_err());
        let newer = fixture(dir.path(), 7, false);
        assert!(load(dir.path(), SID, dir.path())
            .unwrap_err()
            .contains("unsupported"));
        fs::remove_file(newer).unwrap();
        fs::write(&path, "corrupt").unwrap();
        assert!(load(dir.path(), SID, dir.path()).is_err());
    }

    #[test]
    fn home_respects_agent_override_and_node_tilde_and_relative_rules() {
        let dir = tempfile::tempdir().unwrap();
        let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let mut env = HashMap::from([(key.to_string(), dir.path().display().to_string())]);
        assert_eq!(home(&env, dir.path()).unwrap(), dir.path().join(".dsh"));
        env.insert("DSH_HOME".into(), "~/other".into());
        assert_eq!(home(&env, dir.path()).unwrap(), dir.path().join("other"));
        env.insert("DSH_HOME".into(), "relative".into());
        assert_eq!(home(&env, dir.path()).unwrap(), dir.path().join("relative"));
    }

    /// Supply an isolated DSH_HOME with a prepared ACP profile and a transcript
    /// produced by the official Dsh encoder. No model prompt or API key is used.
    #[tokio::test]
    #[ignore = "requires AGENTERO_DSH_HISTORY_{NODE,ENTRY,HOME,CWD} and the official Dsh npm package"]
    async fn official_dsh_history_survives_fresh_processes() {
        use crate::features::agent::models::AgentDescriptor;
        let required = |name| std::env::var(format!("AGENTERO_DSH_HISTORY_{name}")).unwrap();
        let cwd = PathBuf::from(required("CWD"));
        let desc: AgentDescriptor = serde_json::from_value(json!({
            "id":"dsh-history-test", "name":"Dsh", "template":"dsh",
            "command":required("NODE"),
            "args":[required("ENTRY"),"--profile","acp"],
            "env":{"DSH_HOME":required("HOME")}
        }))
        .unwrap();
        let listed = super::super::list_acp_sessions(&desc, cwd.clone(), None, None)
            .await
            .unwrap();
        assert!(listed.supported);
        assert!(listed.sessions.iter().any(|item| item.session_id == SID));
        for _ in 0..2 {
            // Each call creates and then tears down a fresh official ACP process.
            let restored = super::super::load_acp_session(&desc, SID.into(), cwd.clone(), None)
                .await
                .unwrap();
            assert_eq!(
                restored
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>(),
                [
                    "first question",
                    "first answer",
                    "second question",
                    "second answer"
                ]
            );
            assert_eq!(restored.title.as_deref(), Some("my conversation"));
        }
    }
}
