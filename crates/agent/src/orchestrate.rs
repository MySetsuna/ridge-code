use crate::brain::{circuit_broken, completion_blocked, explore_exhausted, over_budget, stalled};
use crate::communication::{
    in_process_exchange, AgentEnvelope, AgentError, AgentHello, AgentMessage, AgentProtocolError,
    AgentResponse, AgentRole, AgentStatus, AgentTask,
};
use crate::context::context_rotted;
use crate::graph::{build_llm_agent, build_llm_agent_read_only};
use crate::knowledge::{provider_failure_label, Agents};
use crate::route::{RouteAudit, RouteRequest, RouteRole};
use crate::state::{AgentState, MAX_DISPATCH_BATCHES, MAX_STEPS};
use langgraph::{Checkpoint, Checkpointer, CompiledGraph, GraphError, RunConfig, StreamEvent};
use provider::{CompletionRequest, LlmProvider, Message, Role};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 规划器(M5 起步):让 provider 把一个目标拆成有序子任务(JSON 数组)。
/// 解析失败/模型出错 → **降级**为把整个目标当单个子任务(绝不返回空,循环有活干)。
///
/// 子任务本身可交给 [`build_llm_agent`] 逐个执行;彼此独立的还能靠引擎的 fan-out 并行跑。
async fn plan_attempt(provider: &dyn LlmProvider, task: &str) -> Result<Vec<String>, String> {
    let req = CompletionRequest {
        messages: vec![
            Message::new(
                Role::System,
                "Break the user's goal into 2-5 ordered, concrete subtasks. \
                 Reply ONLY a JSON array of strings, nothing else.",
            ),
            Message::new(Role::User, task.to_string()),
        ],
        tools: vec![],
    };
    let text = provider
        .complete(&req)
        .await
        .map_err(|error| provider_failure_label(error.as_ref()))?
        .text;
    Ok(parse_subtasks(&text).unwrap_or_else(|| vec![task.to_string()]))
}

pub async fn plan(provider: &dyn LlmProvider, task: &str) -> Vec<String> {
    plan_attempt(provider, task)
        .await
        .unwrap_or_else(|_| vec![task.to_string()])
}

struct TeammateOutcome {
    approved: bool,
    steps: usize,
    tokens: usize,
}

async fn run_teammate_via_protocol(
    provider: Arc<dyn LlmProvider>,
    task: &str,
    correlation_id: &str,
) -> Result<TeammateOutcome, GraphError> {
    let request = AgentEnvelope::task(
        format!("{correlation_id}:task"),
        "main",
        "teammate",
        correlation_id,
        AgentTask::new(
            task,
            true,
            vec!["read_file".to_string(), "search".to_string()],
            MAX_STEPS,
        ),
    );
    let response = in_process_exchange(
        AgentHello::guarded("main", AgentRole::Planner),
        AgentHello::read_only("teammate", AgentRole::Worker),
        request,
        |incoming| async move {
            let correlation_id = incoming.correlation_id.clone();
            let parent_id = incoming.message_id.clone();
            let from = incoming.to.clone();
            let to = incoming.from.clone();
            let AgentMessage::Task(payload) = incoming.message else {
                return Err(AgentProtocolError::Invalid(
                    "teammate expected Task".to_string(),
                ));
            };
            let app = build_llm_agent_read_only(provider)
                .map_err(|error| AgentProtocolError::Handler(error.to_string()))?;
            let outcome = app
                .invoke(AgentState::new(payload.task))
                .await
                .map_err(|error| AgentProtocolError::Handler(error.to_string()))?;
            Ok(AgentEnvelope::response(
                format!("{correlation_id}:response"),
                from,
                to,
                correlation_id,
                AgentResponse {
                    status: AgentStatus::Done,
                    approved: outcome.approved,
                    steps: outcome.steps,
                    tokens: outcome.total_tokens,
                    summary: outcome.messages.last().cloned().unwrap_or_default(),
                    modified_files: outcome.modified_files.into_iter().collect(),
                },
            )
            .with_parent(parent_id))
        },
    )
    .await
    .map_err(|error| GraphError::Join(error.to_string()))?;
    match response.message {
        AgentMessage::Response(result) => Ok(TeammateOutcome {
            approved: result.approved,
            steps: result.steps,
            tokens: result.tokens,
        }),
        AgentMessage::Error(AgentError { message, .. }) => Err(GraphError::Join(message)),
        _ => Err(GraphError::Join(
            "teammate returned unexpected message".to_string(),
        )),
    }
}

/// 一轮任务的**停机原因**(loop engineering:让「为什么停」成为机器可判的确定性信号,
/// 而非只知道停了)。`Approved`=确定性验证通过(成功);其余三种是护栏熔断(**响亮失败**):
/// `Budget` 超 token 预算、`Stall` 连续无进展、`StepCap` 到硬回合上限;`Unverified`=模型收尾但未获通过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaltReason {
    Approved,
    Budget,
    Stall,
    StepCap,
    /// **约束违反**(奖励黑客):试图删/清空受保护路径(测试)等 —— 被守卫硬拦。
    ConstraintBreach,
    /// **上下文腐烂**:压缩后上下文仍超硬上限(单条巨消息压不掉),继续只烧预算/降智。
    ContextRot,
    /// **熔断**:连续工具/provider 报错达 [`MAX_ERR_STREAK`],无人值守下提前停机防烧预算。
    CircuitBroken,
    Unverified,
}

impl HaltReason {
    pub fn as_str(self) -> &'static str {
        match self {
            HaltReason::Approved => "approved",
            HaltReason::Budget => "budget_exceeded",
            HaltReason::Stall => "no_progress",
            HaltReason::StepCap => "step_cap",
            HaltReason::ConstraintBreach => "constraint_breach",
            HaltReason::ContextRot => "context_rot",
            HaltReason::CircuitBroken => "circuit_broken",
            HaltReason::Unverified => "unverified",
        }
    }
    /// 成功(确定性验证通过)才 true;熔断/违约/未验证都是失败,供调用方给非零退出码。
    pub fn is_success(self) -> bool {
        matches!(self, HaltReason::Approved)
    }
}

/// 据终态判定停机原因。优先级(高→低):成功、超预算(经济护栏最该被看见)、**约束违反**(奖励黑客,
/// 安全须显)、**上下文腐烂**(结构性根因)、**熔断**(连错症状)、无进展(输出停滞)、回合上限(通用耗尽)、未验证。
/// 「更根因/更具体者优先」:同为失败终态时,给最有诊断价值的标签(喂 signal 复利)。
pub fn halt_reason(s: &AgentState) -> HaltReason {
    if s.approved && !completion_blocked(s) {
        HaltReason::Approved
    } else if over_budget(s) {
        HaltReason::Budget
    } else if s
        .last_error
        .as_deref()
        .is_some_and(|e| e.contains("constraint"))
    {
        HaltReason::ConstraintBreach
    } else if context_rotted(s) {
        HaltReason::ContextRot
    } else if circuit_broken(s) {
        HaltReason::CircuitBroken
    } else if stalled(s) || explore_exhausted(s) {
        // 同标签 no_progress:输出重复 或 纯侦察耗尽(一直查不落盘),用户侧语义都是「没推进」
        HaltReason::Stall
    } else if s.steps >= MAX_STEPS {
        HaltReason::StepCap
    } else {
        HaltReason::Unverified
    }
}

/// 把一轮任务落成**标准存储库**的一条 run:`<run_dir>/manifest.json`(结构化结论:任务/是否通过/
/// 停机原因/步数/token)+ `trace.json`(完整审计轨迹)。相比旧的「cwd 平铺 trace.json 每轮覆盖」,
/// 每 run 独立目录 → 审计历史不再互相冲掉,是 loop engineering 里跨 run 复利的物理底座。
///
/// ponytail: 只落 manifest+trace 这两样**真正被产出**的东西。跨 loop 复利单元 signal 已落地(iter-16),
/// 但存**项目级** `.ridge/signals`(跨 run 共享 → 才复利),非 run 级子目录;溯源靠 signal 的 `source` 字段回指本 run。
pub fn write_run(out: &AgentState, run_dir: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    let dir = run_dir.as_ref();
    let reason = halt_reason(out);
    // 可观测(iter-44):run 收尾一条 info 事件(停机原因/步数/token)。
    tracing::info!(
        reason = %reason.as_str(),
        steps = out.steps,
        tokens = out.total_tokens,
        approved = out.approved && !completion_blocked(out),
        "run complete"
    );
    std::fs::create_dir_all(dir)?;
    write_run_progress(
        out,
        dir,
        if out.approved && !completion_blocked(out) {
            "completed"
        } else {
            "stopped"
        },
    )?;
    write_trace(out, dir.join("trace.json"))
}

/// Human-readable durable heartbeat. Checkpoints carry the full state; this
/// small manifest lets a watchdog inspect liveness without parsing history.
pub fn write_run_progress(
    out: &AgentState,
    run_dir: impl AsRef<std::path::Path>,
    status: &str,
) -> std::io::Result<()> {
    let dir = run_dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let complete = out.approved && !completion_blocked(out);
    let unfinished_todos = out
        .todos
        .iter()
        .filter(|todo| todo.status.trim() != "completed")
        .count();
    let effective_status = progress_status(status, complete);
    let phase = progress_phase(out, complete, unfinished_todos);
    let next_action = progress_next_action(out, complete, unfinished_todos);
    let modified_files = out
        .modified_files
        .iter()
        .take(256)
        .cloned()
        .collect::<Vec<_>>();
    let todos = out
        .todos
        .iter()
        .take(256)
        .map(|todo| {
            serde_json::json!({
                "content": &todo.content,
                "status": &todo.status,
            })
        })
        .collect::<Vec<_>>();
    let manifest = serde_json::json!({
        "schema_version": 1,
        "status": effective_status,
        "task": out.task,
        "approved": complete,
        "completion_blocked": completion_blocked(out),
        "pending_call": out.pending_call.as_ref().map(|call| call.name.clone()),
        "todo_total": out.todos.len(),
        "todo_pending": unfinished_todos,
        "todos": todos,
        "halt_reason": halt_reason(out).as_str(),
        "phase": phase,
        "next_action": next_action,
        "step": out.steps,
        "steps": out.steps,
        "tokens": out.total_tokens,
        "stall": out.stall,
        "err_streak": out.err_streak,
        "explore_streak": out.explore_streak,
        "dispatch_batches_used": out.dispatch_wave_count(),
        "dispatch_batches_remaining": MAX_DISPATCH_BATCHES
            .saturating_sub(out.dispatch_wave_count()),
        "codegraph_unavailable": out.codegraph_unavailable,
        "modified_files": modified_files,
        "modified_files_count": out.modified_files.len(),
        "blocker": out.last_error,
        "updated_at_ms": unix_millis(),
        "owner_pid": std::process::id(),
    });
    let json = serde_json::to_string_pretty(&manifest).map_err(std::io::Error::other)?;
    atomic_write(dir.join("manifest.json"), json.as_bytes())
}

fn progress_status(status: &str, complete: bool) -> &str {
    if status == "completed" && !complete {
        "stopped"
    } else {
        status
    }
}

fn progress_phase(out: &AgentState, complete: bool, unfinished_todos: usize) -> &'static str {
    if complete {
        "completed"
    } else if out.pending_call.is_some() {
        "action_pending"
    } else if unfinished_todos > 0 {
        "completion_blocked"
    } else if out.last_action.as_deref() == Some("finish") {
        "verifying"
    } else if out.explore_handoff {
        "action_handoff"
    } else if out.last_action.is_some() {
        "acting"
    } else {
        "reasoning"
    }
}

fn progress_next_action(
    out: &AgentState,
    complete: bool,
    unfinished_todos: usize,
) -> Option<String> {
    if complete {
        None
    } else if let Some(call) = &out.pending_call {
        Some(call.name.clone())
    } else if unfinished_todos > 0 {
        Some("complete_todos".to_string())
    } else if out.explore_handoff {
        Some("edit_or_verify".to_string())
    } else if out.last_action.as_deref() == Some("finish") {
        Some("verify".to_string())
    } else {
        Some("reason".to_string())
    }
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// One latest checkpoint per active run. The graph engine remains generic;
/// this agent-level adapter makes process restart recovery durable and bounded.
pub struct DurableCheckpointer {
    run_dir: PathBuf,
    path: PathBuf,
    error: Arc<Mutex<Option<String>>>,
}

impl DurableCheckpointer {
    pub fn new(run_dir: impl Into<PathBuf>) -> Self {
        let run_dir = run_dir.into();
        let path = run_dir.join("checkpoint.json");
        Self {
            run_dir,
            path,
            error: Arc::new(Mutex::new(None)),
        }
    }

    fn record_error(&self, error: impl std::fmt::Display) {
        if let Ok(mut slot) = self.error.lock() {
            if slot.is_none() {
                *slot = Some(error.to_string());
            }
        }
    }

    fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl Checkpointer<AgentState> for DurableCheckpointer {
    fn save(&self, checkpoint: Checkpoint<AgentState>) {
        if let Err(error) = std::fs::create_dir_all(&self.run_dir) {
            self.record_error(error);
            return;
        }
        let checkpoint = Checkpoint {
            step: checkpoint.step,
            frontier: checkpoint.frontier,
            state: bounded_checkpoint_state(checkpoint.state),
        };
        let mut body = match serde_json::to_vec(&checkpoint) {
            Ok(body) => body,
            Err(error) => {
                self.record_error(error);
                return;
            }
        };
        body.push(b'\n');
        if let Err(error) = atomic_write(&self.path, &body) {
            self.record_error(error);
            return;
        }
        if let Err(error) = write_run_progress(&checkpoint.state, &self.run_dir, "running") {
            self.record_error(error);
        }
    }
}

fn bounded_checkpoint_state(mut state: AgentState) -> AgentState {
    state.history = bounded_resume_history(state.history);
    if state.messages.len() > 32 {
        state.messages = state.messages.split_off(state.messages.len() - 32);
    }
    // Presentation history is already persisted in the completed run trace;
    // it is not required to resume graph routing or provider context.
    state.display_messages.clear();
    if state.modified_files.len() > 256 {
        state.modified_files = state.modified_files.iter().take(256).cloned().collect();
    }
    state
}

fn bounded_resume_history(history: Vec<Message>) -> Vec<Message> {
    let history = crate::context::compact_history(history, 8);
    if history.len() <= 16 {
        return history;
    }
    let tail_start = history.len() - 15;
    let first_user = history
        .iter()
        .take(tail_start)
        .find(|message| message.role == Role::User)
        .cloned();
    let mut bounded = Vec::with_capacity(16);
    if let Some(first_user) = first_user {
        bounded.push(first_user);
    }
    bounded.extend(history.into_iter().skip(tail_start));
    provider::repair_tool_history(&bounded)
}

pub fn load_durable_checkpoint(
    run_dir: impl AsRef<Path>,
    task: &str,
) -> std::io::Result<Option<Checkpoint<AgentState>>> {
    let path = run_dir.as_ref().join("checkpoint.json");
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let checkpoint =
        serde_json::from_str::<Checkpoint<AgentState>>(&text).map_err(std::io::Error::other)?;
    if !task.is_empty() && checkpoint.state.task != task {
        return Ok(None);
    }
    Ok(Some(checkpoint))
}

pub fn active_run_dir() -> PathBuf {
    Path::new(".ridge").join("runs").join("active")
}

/// Stable per-task active directory. A cancelled task must remain resumable
/// when a queued task starts; one shared checkpoint would overwrite it.
pub fn active_run_dir_for(task: &str) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in task.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    active_run_dir().join(format!("{hash:016x}"))
}

/// A live manifest prevents a second process from executing the same goal;
/// an old heartbeat is treated as recoverable after a process crash.
pub fn durable_run_is_live(task: &str) -> bool {
    let path = active_run_dir_for(task).join("manifest.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    if manifest["status"] != "running" {
        return false;
    }
    manifest["owner_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .is_some_and(process_is_alive)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use std::ffi::c_void;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const STILL_ACTIVE: u32 = 259;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
        fn CloseHandle(object: *mut c_void) -> i32;
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut exit_code = 0;
    let alive =
        unsafe { GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE };
    unsafe {
        CloseHandle(handle);
    }
    alive
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(any(windows, unix)))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

/// Mark the latest checkpoint as cancelled without deleting it. The next
/// invocation can still resume the same task from its last completed step.
pub fn mark_durable_interrupted(run_dir: impl AsRef<Path>, reason: &str) -> std::io::Result<()> {
    mark_durable_status(run_dir, reason, "interrupted")
}

pub fn mark_durable_cancelled(run_dir: impl AsRef<Path>, reason: &str) -> std::io::Result<()> {
    mark_durable_status(run_dir, reason, "cancelled")
}

fn mark_durable_status(
    run_dir: impl AsRef<Path>,
    reason: &str,
    status: &str,
) -> std::io::Result<()> {
    let run_dir = run_dir.as_ref();
    let Some(checkpoint) = load_durable_checkpoint(run_dir, "")? else {
        return Ok(());
    };
    let mut state = checkpoint.state;
    state.last_error = Some(reason.chars().take(2_000).collect());
    DurableCheckpointer::new(run_dir).save(Checkpoint {
        step: checkpoint.step,
        frontier: checkpoint.frontier,
        state: state.clone(),
    });
    write_run_progress(&state, run_dir, status)
}

/// Invoke one agent run with a latest durable checkpoint and heartbeat.
/// Recovery is task-scoped, bounded, and never starts a different task from
/// an old checkpoint.
pub async fn invoke_durable(
    app: &CompiledGraph<AgentState>,
    state: AgentState,
    config: &RunConfig,
    tx: Option<&tokio::sync::mpsc::UnboundedSender<StreamEvent<AgentState>>>,
) -> Result<AgentState, GraphError> {
    let run_dir = active_run_dir_for(&state.task);
    invoke_durable_at(app, state, config, tx, run_dir).await
}

pub async fn invoke_durable_at(
    app: &CompiledGraph<AgentState>,
    state: AgentState,
    config: &RunConfig,
    tx: Option<&tokio::sync::mpsc::UnboundedSender<StreamEvent<AgentState>>>,
    run_dir: impl Into<PathBuf>,
) -> Result<AgentState, GraphError> {
    let run_dir = run_dir.into();
    let checkpointer = DurableCheckpointer::new(run_dir.clone());
    let resume = match load_durable_checkpoint(&run_dir, &state.task) {
        Ok(checkpoint) => checkpoint,
        Err(error) => {
            let message = format!("durable checkpoint unreadable: {error}");
            let mut blocked = state;
            blocked.last_error = Some(message.clone());
            let _ = write_run_progress(&blocked, &run_dir, "blocked");
            return Err(GraphError::Join(message));
        }
    }
    .filter(|checkpoint| {
        (!checkpoint.state.approved || completion_blocked(&checkpoint.state))
            && !checkpoint.frontier.is_empty()
    });
    let result = match resume {
        Some(checkpoint) => {
            tracing::info!(step = checkpoint.step, task = %checkpoint.state.task, "resuming durable run");
            app.resume(checkpoint, config, Some(&checkpointer), tx)
                .await
        }
        None => {
            app.invoke_with(state, config, Some(&checkpointer), tx)
                .await
        }
    };
    if let Some(error) = checkpointer.take_error() {
        let message = format!("durable checkpoint failed: {error}");
        let _ = mark_durable_interrupted(&run_dir, &message);
        return Err(GraphError::Join(message));
    }
    match &result {
        Ok(out) => {
            let _ = write_run(out, &run_dir);
        }
        Err(error) => {
            let _ = mark_durable_interrupted(&run_dir, &format!("run interrupted: {error}"));
        }
    }
    result
}

/// 写一轮的审计轨迹到 `trace.json`(DoD⑥:客观证据,含工具输出/退出码 + 多轮 history)。密钥不入 trace。
pub fn write_trace(out: &AgentState, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    let complete = out.approved && !completion_blocked(out);
    let record = serde_json::json!({
        "task": out.task,
        "approved": complete,
        "completion_blocked": completion_blocked(out),
        "pending_call": out.pending_call.as_ref().map(|call| call.name.clone()),
        "todos": out.todos,
        "steps": out.steps,
        "tokens": out.total_tokens,
        "trace": out.messages,   // 人读轨迹(含 act 的 exit code / 工具输出)
        "display_trace": out.display_messages,
        "history": out.history,  // 模型面向多轮(含 role=tool 结果)
    });
    let json = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
    atomic_write(path, json.as_bytes())
}

fn atomic_write(path: impl AsRef<std::path::Path>, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let path = path.as_ref();
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temp = parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(body)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    fn wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }

    let from = wide(from);
    let to = wide(to);
    let flags = 0x0000_0001 | 0x0000_0008;
    let result = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// 从模型输出里抠出首个 `[` 到末个 `]` 的 JSON 数组(容忍模型包裹的解释文字)。
fn parse_subtasks(text: &str) -> Option<Vec<String>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    let arr: Vec<String> = serde_json::from_str(text.get(start..=end)?).ok()?;
    (!arr.is_empty()).then_some(arr)
}

/// 一个子任务的执行结果。
#[derive(Clone, Debug)]
pub struct SubtaskResult {
    pub task: String,
    pub approved: bool,
    pub steps: usize,
    pub tokens: usize,
    /// Structured provider/model choice; absent for the legacy fixed-provider API.
    pub route: Option<RouteAudit>,
}

/// 规划-执行的聚合报告。
#[derive(Clone, Debug)]
pub struct PlanReport {
    pub subtasks: Vec<SubtaskResult>,
    /// 全部子任务都通过才算整体通过。
    pub approved: bool,
    pub total_tokens: usize,
    pub total_steps: usize,
    /// Planner choice for the routed API; absent for the legacy fixed-provider API.
    pub planner_route: Option<RouteAudit>,
}

/// **规划 + 执行**(orchestrator-workers,M5 完整版):
/// `planner`(通常是强模型)把目标拆成子任务,`worker` 逐个执行,聚合结果。
/// 成本杠杆:强模型只管规划,弱模型扛执行量(planner ≠ worker)。
///
/// 目前**串行**执行(子任务常有依赖);彼此独立的子任务可改用 `tokio::spawn` + `join_all`
/// 并行(引擎/运行时已支持),这里先要正确性。
pub async fn run_planned(
    planner: Arc<dyn LlmProvider>,
    worker: Arc<dyn LlmProvider>,
    task: &str,
) -> Result<PlanReport, GraphError> {
    let subtasks = plan(planner.as_ref(), task).await;
    let mut results = Vec::with_capacity(subtasks.len());
    let mut total_tokens = 0;
    let mut total_steps = 0;
    let mut approved = true;

    for sub in subtasks {
        let app = build_llm_agent(worker.clone())?;
        let out = app.invoke(AgentState::new(sub.clone())).await?;
        approved &= out.approved;
        total_tokens += out.total_tokens;
        total_steps += out.steps;
        results.push(SubtaskResult {
            task: sub,
            approved: out.approved,
            steps: out.steps,
            tokens: out.total_tokens,
            route: None,
        });
    }

    Ok(PlanReport {
        subtasks: results,
        approved,
        total_tokens,
        total_steps,
        planner_route: None,
    })
}

/// Routed planner/teammate execution. Selection happens before each bounded
/// graph invocation; execution remains the existing serial, verified loop.
pub async fn run_planned_routed(
    agents: &Agents,
    main: Arc<dyn LlmProvider>,
    task: &str,
) -> Result<PlanReport, GraphError> {
    let planner_request = RouteRequest::from_task(task, RouteRole::Planner);
    let planner = agents.select_provider(&planner_request, main.clone());
    let mut planner_route = planner.decision.audit(RouteRole::Planner);
    let subtasks = match plan_attempt(planner.provider.as_ref(), task).await {
        Ok(subtasks) => subtasks,
        Err(first_failure) if planner_route.selected.is_some() => {
            planner_route.used_fallback = true;
            planner_route.reason = format!(
                "{}; selected provider failed ({first_failure}), deterministic main-provider fallback",
                planner_route.reason
            );
            match plan_attempt(main.as_ref(), task).await {
                Ok(subtasks) => subtasks,
                Err(fallback_failure) => {
                    planner_route.reason = format!(
                        "{}; main-provider fallback failed ({fallback_failure}), using original task",
                        planner_route.reason
                    );
                    vec![task.to_string()]
                }
            }
        }
        Err(_) => vec![task.to_string()],
    };
    let mut results = Vec::with_capacity(subtasks.len());
    let mut total_tokens = 0;
    let mut total_steps = 0;
    let mut approved = true;

    for sub in subtasks {
        let worker_request = RouteRequest::from_task(&sub, RouteRole::Teammate);
        let worker = agents.select_provider(&worker_request, main.clone());
        let mut worker_route = worker.decision.audit(RouteRole::Teammate);
        let out = match run_teammate_via_protocol(
            worker.provider.clone(),
            &sub,
            &format!("teammate:{index}", index = results.len()),
        )
        .await
        {
            Ok(out) => out,
            Err(first_failure) if worker_route.selected.is_some() => {
                worker_route.used_fallback = true;
                worker_route.reason = format!(
                    "{}; selected provider failed ({}), deterministic main-provider fallback",
                    worker_route.reason,
                    provider_failure_label(&first_failure)
                );
                run_teammate_via_protocol(
                    main.clone(),
                    &sub,
                    &format!("teammate:{}:fallback", results.len()),
                )
                .await?
            }
            Err(error) => return Err(error),
        };
        approved &= out.approved;
        total_tokens += out.tokens;
        total_steps += out.steps;
        results.push(SubtaskResult {
            task: sub,
            approved: out.approved,
            steps: out.steps,
            tokens: out.tokens,
            route: Some(worker_route),
        });
    }

    Ok(PlanReport {
        subtasks: results,
        approved,
        total_tokens,
        total_steps,
        planner_route: Some(planner_route),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::CONTEXT_ROT_TOKENS;
    use crate::*;
    use provider::ToolCall;

    struct AlwaysFailProvider;

    #[async_trait::async_trait]
    impl LlmProvider for AlwaysFailProvider {
        async fn complete(
            &self,
            _req: &CompletionRequest,
        ) -> Result<provider::Completion, provider::ProviderError> {
            Err("http 503: unavailable-body".into())
        }
    }

    // 一个「永不收工、每轮都调同一个失败命令」的 provider 步骤,带可配的 token 用量。
    fn stuck_step(tokens: u32) -> provider::Completion {
        provider::Completion {
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "exit 1"}),
            }],
            usage: provider::Usage {
                prompt_tokens: tokens,
                completion_tokens: 0,
            },
            ..Default::default()
        }
    }

    /// 成本护栏:每轮烧 token,预算耗尽即熔断,不跑到回合上限。
    #[tokio::test]
    async fn budget_breaker_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(100)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app
            .invoke(AgentState::new("loop").with_budget(250))
            .await
            .unwrap();

        assert!(!out.approved, "must not fake success");
        assert!(out.total_tokens >= 250, "hit budget: {}", out.total_tokens);
        assert!(
            out.steps < MAX_STEPS,
            "budget熔断应早于回合上限: steps={}",
            out.steps
        );
    }

    /// 无进展检测:工具输出连续 MAX_STALL 轮不变即熔断,不跑到回合上限。
    #[tokio::test]
    async fn no_progress_detection_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(0)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app.invoke(AgentState::new("stuck")).await.unwrap();

        assert!(!out.approved);
        assert!(out.stall >= MAX_STALL, "stall={}", out.stall);
        assert!(
            out.steps < MAX_STEPS,
            "no-progress熔断应早于回合上限: steps={}",
            out.steps
        );
    }

    /// 纯侦察耗尽:每轮 read 不同文件 → stall 不触发,但 explore_streak 触顶后 soft-stop(no_progress),
    /// 不得烧到 MAX_STEPS 后再「重新触发一轮全库侦察」。
    #[tokio::test]
    async fn explore_thrash_stops_before_step_cap() {
        use provider::{Completion, ScriptedProvider, ToolCall};
        let dir = std::env::temp_dir().join(format!("ridge_explore_thrash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // MAX_EXPLORE 次不同读 + wrapup 补全;多备几条防边界
        let n = MAX_EXPLORE + 4;
        let mut script = Vec::with_capacity(n + 1);
        for i in 0..n {
            let p = dir.join(format!("f{i}.txt"));
            std::fs::write(&p, format!("content-{i}")).unwrap();
            script.push(Completion {
                tool_calls: vec![ToolCall {
                    id: format!("r{i}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": p.to_str().unwrap()}),
                }],
                ..Default::default()
            });
        }
        script.push(Completion {
            text: "已定位,建议下一步写改".into(),
            ..Default::default()
        });
        let app = build_llm_agent(Arc::new(ScriptedProvider::new(script))).unwrap();
        let out = app
            .invoke(AgentState::new("fix the bug then edit"))
            .await
            .unwrap();
        assert!(!out.approved, "只读侦察不得伪造成功");
        assert!(
            out.explore_streak >= MAX_EXPLORE,
            "explore_streak={}",
            out.explore_streak
        );
        assert_eq!(halt_reason(&out), HaltReason::Stall);
        assert!(
            out.steps < MAX_STEPS,
            "侦察熔断应远早于 step_cap: steps={}",
            out.steps
        );
        assert!(
            out.steps <= MAX_EXPLORE + 6,
            "应在触顶后很快 wrapup, steps={}",
            out.steps
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 停机原因分类:据终态确定性判定,优先级 成功 > 预算 > 无进展 > 回合上限 > 未验证。
    #[test]
    fn halt_reason_classifies_each_outcome() {
        let approved = AgentState {
            approved: true,
            ..Default::default()
        };
        assert_eq!(halt_reason(&approved), HaltReason::Approved);
        assert!(halt_reason(&approved).is_success());

        let incomplete = AgentState {
            approved: true,
            todos: vec![Todo {
                content: "still running".into(),
                status: "in_progress".into(),
            }],
            ..Default::default()
        };
        assert_eq!(halt_reason(&incomplete), HaltReason::Unverified);
        assert!(!halt_reason(&incomplete).is_success());

        let budget = AgentState {
            budget_tokens: 100,
            total_tokens: 100,
            ..Default::default()
        };
        assert_eq!(halt_reason(&budget), HaltReason::Budget);

        let stall = AgentState {
            stall: MAX_STALL,
            ..Default::default()
        };
        assert_eq!(halt_reason(&stall), HaltReason::Stall);

        // 纯侦察耗尽:输出每轮不同 stall 不触发,但 explore_streak 触顶 → 同 no_progress
        let explore = AgentState {
            explore_streak: MAX_EXPLORE,
            ..Default::default()
        };
        assert_eq!(halt_reason(&explore), HaltReason::Stall);

        let cap = AgentState {
            steps: MAX_STEPS,
            ..Default::default()
        };
        assert_eq!(halt_reason(&cap), HaltReason::StepCap);

        // 约束违反(奖励黑客)由 last_error 分类,优先于回合上限。
        let breach = AgentState {
            steps: MAX_STEPS,
            last_error: Some("BLOCKED (constraint): 拒绝清空受保护路径".into()),
            ..Default::default()
        };
        assert_eq!(halt_reason(&breach), HaltReason::ConstraintBreach);

        // 熔断:连错达阈值 → circuit_broken,优先于回合上限(错误内容每轮不同、stall 不触发时兜底)。
        let circuit = AgentState {
            steps: MAX_STEPS,
            err_streak: MAX_ERR_STREAK,
            ..Default::default()
        };
        assert_eq!(halt_reason(&circuit), HaltReason::CircuitBroken);

        // 上下文腐烂:压缩后单条巨消息仍超硬上限 → context_rot,优先于熔断(结构性根因)。
        let big = "字".repeat(CONTEXT_ROT_TOKENS + 1); // 每 CJK 字≈1tok,单条即超硬上限,压不掉
        let rot = AgentState {
            steps: MAX_STEPS,
            err_streak: MAX_ERR_STREAK,
            history: vec![Message::user(big)],
            ..Default::default()
        };
        assert_eq!(halt_reason(&rot), HaltReason::ContextRot);

        // 熔断/违约都非成功 → 调用方据此给非零退出码。
        for r in [
            HaltReason::Budget,
            HaltReason::Stall,
            HaltReason::StepCap,
            HaltReason::ConstraintBreach,
            HaltReason::ContextRot,
            HaltReason::CircuitBroken,
            HaltReason::Unverified,
        ] {
            assert!(!r.is_success(), "{} 不应算成功", r.as_str());
        }
    }

    /// 熔断早停:连续报错累计到 MAX_ERR_STREAK 即命中 must_stop(早于回合上限)。
    #[test]
    fn circuit_breaks_before_step_cap() {
        let broken = AgentState {
            err_streak: MAX_ERR_STREAK,
            steps: 1, // 远未到回合上限
            ..Default::default()
        };
        assert!(circuit_broken(&broken));
        assert!(must_stop(&broken), "连错达阈值应触发停机,不必跑到回合上限");

        let ok = AgentState {
            err_streak: MAX_ERR_STREAK - 1,
            steps: 1,
            ..Default::default()
        };
        assert!(!must_stop(&ok), "未达连错阈值不应停机");
    }

    /// 标准存储库:一轮任务落成独立 run 目录,含 manifest.json(结构化结论)+ trace.json(完整轨迹)。
    #[test]
    fn write_run_creates_per_run_dir_with_manifest_and_trace() {
        let dir = std::env::temp_dir().join("ridge_write_run_test_1");
        let _ = std::fs::remove_dir_all(&dir); // 清上一次残留,保证干净
        let state = AgentState {
            task: "查天气".into(),
            steps: MAX_STEPS, // 未通过 + 到回合上限 → halt_reason=step_cap
            total_tokens: 42,
            ..Default::default()
        };
        write_run(&state, &dir).unwrap();

        let manifest = dir.join("manifest.json");
        let trace = dir.join("trace.json");
        assert!(manifest.exists(), "manifest.json 应物理生成");
        assert!(trace.exists(), "trace.json 应物理生成");

        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(m["task"], "查天气");
        assert_eq!(m["approved"], false);
        assert_eq!(m["halt_reason"], "step_cap");
        assert_eq!(m["steps"], MAX_STEPS);
        assert_eq!(m["tokens"], 42);
        assert_eq!(m["phase"], "reasoning");
        assert_eq!(m["status"], "stopped");
        assert!(m["updated_at_ms"].as_u64().is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_manifest_exposes_next_action_and_bounded_facts() {
        let dir = std::env::temp_dir().join("ridge_progress_manifest_test_1");
        let _ = std::fs::remove_dir_all(&dir);
        let state = AgentState {
            task: "maintain project".into(),
            explore_handoff: true,
            explore_streak: MAX_EXPLORE,
            modified_files: ["src/lib.rs".to_string()].into_iter().collect(),
            last_error: Some("provider request timed out".into()),
            ..Default::default()
        };
        write_run_progress(&state, &dir, "running").unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["phase"], "action_handoff");
        assert_eq!(manifest["next_action"], "edit_or_verify");
        assert_eq!(manifest["status"], "running");
        assert_eq!(manifest["modified_files"][0], "src/lib.rs");
        assert_eq!(manifest["blocker"], "provider request timed out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_manifest_exposes_completion_blockers_and_never_marks_them_complete() {
        let dir = std::env::temp_dir().join("ridge_progress_completion_blocker_test_1");
        let _ = std::fs::remove_dir_all(&dir);
        let state = AgentState {
            task: "finish durable report".into(),
            approved: true,
            todos: vec![Todo {
                content: "add regression test".into(),
                status: "pending".into(),
            }],
            pending_call: Some(ToolCall {
                id: "call-3".into(),
                name: "run_shell".into(),
                arguments: serde_json::json!({"command": "cargo test"}),
            }),
            ..Default::default()
        };
        write_run_progress(&state, &dir, "completed").unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["status"], "stopped");
        assert_eq!(manifest["approved"], false);
        assert_eq!(manifest["completion_blocked"], true);
        assert_eq!(manifest["pending_call"], "run_shell");
        assert_eq!(manifest["todo_total"], 1);
        assert_eq!(manifest["todo_pending"], 1);
        assert_eq!(manifest["next_action"], "run_shell");
        assert_eq!(manifest["todos"][0]["status"], "pending");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_approved_checkpoint_with_live_todo_is_not_a_success() {
        let dir = std::env::temp_dir().join("ridge_stale_approved_checkpoint_test_1");
        let _ = std::fs::remove_dir_all(&dir);
        let mut state = AgentState::new("resume incomplete work");
        state.approved = true;
        state.todos = vec![Todo {
            content: "finish the pending work".into(),
            status: "pending".into(),
        }];
        write_run(&state, &dir).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["approved"], false);
        assert_eq!(manifest["status"], "stopped");
        assert_eq!(manifest["completion_blocked"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn durable_checkpoint_compacts_resume_state_and_keeps_latest_only() {
        let dir = std::env::temp_dir().join("ridge_durable_compaction_test_1");
        let _ = std::fs::remove_dir_all(&dir);
        let writer = DurableCheckpointer::new(&dir);
        let mut state = AgentState::new("long task");
        state.history = (0..64)
            .map(|index| Message::user(format!("history-{index}")))
            .collect();
        state.display_messages = vec!["presentation-only".into()];
        state.messages = (0..64).map(|index| format!("event-{index}")).collect();
        writer.save(Checkpoint {
            step: 4,
            frontier: vec!["reason".into()],
            state,
        });
        let checkpoint = load_durable_checkpoint(&dir, "long task").unwrap().unwrap();
        assert!(checkpoint.state.history.len() <= 16);
        assert!(checkpoint.state.messages.len() <= 32);
        assert!(checkpoint.state.display_messages.is_empty());
        let first_len = std::fs::metadata(dir.join("checkpoint.json"))
            .unwrap()
            .len();
        writer.save(Checkpoint {
            step: 5,
            frontier: vec!["act".into()],
            state: checkpoint.state,
        });
        let latest = load_durable_checkpoint(&dir, "long task").unwrap().unwrap();
        assert_eq!(latest.step, 5);
        assert!(
            std::fs::metadata(dir.join("checkpoint.json"))
                .unwrap()
                .len()
                <= first_len + 256
        );
        assert!(!dir.join(".checkpoint.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn durable_checkpoint_resumes_after_step_limit_without_replaying_reason() {
        use provider::{Completion, ScriptedProvider, ToolCall};

        let dir =
            std::env::temp_dir().join(format!("ridge_durable_restart_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let provider = Arc::new(ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "restart-shell".into(),
                    name: "run_shell".into(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".into(),
                ..Default::default()
            },
        ]));
        let app = build_llm_agent(provider).unwrap();
        let interrupted = invoke_durable_at(
            &app,
            AgentState::new("restartable task"),
            &RunConfig { max_supersteps: 1 },
            None,
            &dir,
        )
        .await;
        assert!(
            interrupted.is_err(),
            "first process must stop at its test cap"
        );
        let checkpoint = load_durable_checkpoint(&dir, "restartable task")
            .unwrap()
            .expect("step-limit must leave a checkpoint");
        assert_eq!(checkpoint.step, 1);
        assert_eq!(checkpoint.frontier, vec!["act"]);
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["status"], "interrupted");
        assert_eq!(manifest["next_action"], "run_shell");

        let resumed = invoke_durable_at(
            &app,
            AgentState::new("restartable task"),
            &RunConfig::default(),
            None,
            &dir,
        )
        .await
        .unwrap();
        assert!(
            resumed.approved,
            "restart must continue from act and verify"
        );
        let final_manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(final_manifest["status"], "completed");
        assert_eq!(final_manifest["approved"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_checkpoint_keeps_next_action_and_clears_live_heartbeat() {
        let dir =
            std::env::temp_dir().join(format!("ridge_durable_cancel_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let writer = DurableCheckpointer::new(&dir);
        writer.save(Checkpoint {
            step: 3,
            frontier: vec!["act".into()],
            state: AgentState::new("cancel me"),
        });
        mark_durable_cancelled(&dir, "cancelled by test").unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["status"], "cancelled");
        assert!(!durable_run_is_live("cancel me"));
        let checkpoint = load_durable_checkpoint(&dir, "cancel me").unwrap().unwrap();
        assert_eq!(checkpoint.frontier, vec!["act"]);
        assert_eq!(
            checkpoint.state.last_error.as_deref(),
            Some("cancelled by test")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M5:规划器把目标拆成子任务(容忍模型包裹的解释文字)。
    #[tokio::test]
    async fn planner_decomposes_goal_into_subtasks() {
        use provider::{Completion, ScriptedProvider};
        let provider = ScriptedProvider::new(vec![Completion {
            text: r#"Sure! ["add fn", "write test", "run cargo test"]"#.to_string(),
            ..Default::default()
        }]);
        let subs = plan(&provider, "implement add").await;
        assert_eq!(subs, vec!["add fn", "write test", "run cargo test"]);
    }

    /// M5:模型没给出可解析的数组 → 降级为单个子任务(绝不返回空)。
    #[tokio::test]
    async fn planner_falls_back_when_unparseable() {
        use provider::{Completion, ScriptedProvider};
        let provider = ScriptedProvider::new(vec![Completion {
            text: "I'm not sure how to break this down".to_string(),
            ..Default::default()
        }]);
        let subs = plan(&provider, "do the thing").await;
        assert_eq!(subs, vec!["do the thing"]);
    }

    /// M5 完整:planner 拆 2 个子任务 → worker 逐个执行到 approved → 聚合整体通过。
    #[tokio::test]
    async fn orchestrator_plans_and_runs_subtasks() {
        use provider::{Completion, ScriptedProvider};

        let planner = ScriptedProvider::new(vec![Completion {
            text: r#"["impl add", "test add"]"#.to_string(),
            ..Default::default()
        }]);
        // worker 被两个子任务共享(串行):每个子任务耗 [跑 exit 0, 收尾] 两个补全。
        let step_pass = || Completion {
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "exit 0"}),
            }],
            ..Default::default()
        };
        let step_done = || Completion {
            text: "done".to_string(),
            ..Default::default()
        };
        let worker =
            ScriptedProvider::new(vec![step_pass(), step_done(), step_pass(), step_done()]);

        let report = run_planned(
            Arc::new(planner),
            Arc::new(worker),
            "implement add with test",
        )
        .await
        .unwrap();

        assert_eq!(report.subtasks.len(), 2);
        assert!(report.approved, "两个子任务都应通过");
        assert!(report.subtasks.iter().all(|s| s.approved));
        assert_eq!(
            report
                .subtasks
                .iter()
                .map(|s| s.task.as_str())
                .collect::<Vec<_>>(),
            vec!["impl add", "test add"]
        );
    }

    #[tokio::test]
    async fn routed_orchestrator_audits_planner_and_teammate_choices() {
        use provider::{Completion, ScriptedProvider};

        let planner: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![Completion {
            text: r#"["inspect"]"#.into(),
            ..Default::default()
        }]));
        let worker: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![Completion {
            text: "done".into(),
            ..Default::default()
        }]));
        let planner_profile = ModelProfile {
            provider: "deep".into(),
            model: "planner".into(),
            kind: "openai".into(),
            context_window: Some(64_000),
            cost_tier: Some(3),
            latency_tier: Some(3),
            supports_tools: Some(false),
            supports_reasoning: Some(true),
            tags: vec!["planning".into()],
        };
        let worker_profile = ModelProfile {
            provider: "fast".into(),
            model: "worker".into(),
            kind: "openai".into(),
            context_window: Some(64_000),
            cost_tier: Some(1),
            latency_tier: Some(1),
            supports_tools: Some(true),
            supports_reasoning: Some(false),
            tags: vec!["execution".into()],
        };
        let agents = Agents {
            defs: Vec::new(),
            providers: std::collections::HashMap::new(),
            route_candidates: vec![
                AgentProvider {
                    profile: planner_profile,
                    provider: planner.clone(),
                },
                AgentProvider {
                    profile: worker_profile,
                    provider: worker.clone(),
                },
            ],
        };

        let report = run_planned_routed(&agents, planner, "design a complex architecture")
            .await
            .unwrap();
        assert_eq!(
            report
                .planner_route
                .as_ref()
                .and_then(|route| route.selected.as_deref()),
            Some("deep::planner")
        );
        assert_eq!(report.subtasks.len(), 1);
        assert_eq!(
            report.subtasks[0]
                .route
                .as_ref()
                .and_then(|route| route.selected.as_deref()),
            Some("fast::worker")
        );
        assert!(report.approved);
        assert!(report.subtasks[0]
            .route
            .as_ref()
            .is_some_and(|route| route.reason.contains("role=teammate")));
    }

    #[tokio::test]
    async fn routed_orchestrator_falls_back_after_planner_and_teammate_failures() {
        use provider::{Completion, ScriptedProvider};

        let main: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![
            Completion {
                text: r#"["inspect"]"#.into(),
                ..Default::default()
            },
            Completion {
                text: "done".into(),
                ..Default::default()
            },
        ]));
        let failing: Arc<dyn LlmProvider> = Arc::new(AlwaysFailProvider);
        let profile = ModelProfile {
            provider: "limited".into(),
            model: "unavailable".into(),
            kind: "openai".into(),
            context_window: Some(64_000),
            cost_tier: Some(1),
            latency_tier: Some(1),
            supports_tools: Some(true),
            supports_reasoning: Some(true),
            tags: vec![],
        };
        let agents = Agents {
            defs: Vec::new(),
            providers: std::collections::HashMap::new(),
            route_candidates: vec![AgentProvider {
                profile,
                provider: failing,
            }],
        };

        let report = run_planned_routed(&agents, main.clone(), "design a complex architecture")
            .await
            .unwrap();
        assert!(report.approved);
        assert!(report
            .planner_route
            .as_ref()
            .is_some_and(|route| route.used_fallback
                && route.reason.contains("http 503")
                && route.reason.contains("main-provider fallback")));
        assert!(report.subtasks[0].route.as_ref().is_some_and(
            |route| route.used_fallback && route.reason.contains("main-provider fallback")
        ));
    }
}
