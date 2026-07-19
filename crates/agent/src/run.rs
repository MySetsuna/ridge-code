//! 任务执行:run_once / headless / 流式渲染 / 产物落盘 / 报告 / demo。
use crate::*;

/// 一次性任务:一律放行,跑完写 run 留痕 + 打印结果。
/// `every=Some(dur)`:**时间触发器**(rung-3 延迟阶梯)—— app 只建一次,按间隔重跑同一任务,
/// 每轮重载 `.ridge/signals`(信号复利)、失败自动落信号,直到 Ctrl-C。是「常驻助手」的最小形态。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_once(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    task: &str,
    budget: usize,
    agents: Arc<agent::Agents>,
    read_only: bool,
    every: Option<std::time::Duration>,
) -> anyhow::Result<()> {
    let bus = null_token_bus();
    // opt-in 自动 signal 抽取器:建 app 前留一把 provider(Arc 克隆廉价),供 run 收尾提炼复利信号。
    let extractor = signal_extract_enabled().then(|| provider.clone());
    let app = build_llm_agent_full(
        provider,
        mcp,
        Arc::new(AutoApprove),
        skills,
        bus.clone(),
        agents,
        read_only,
    )?;
    if let Some(dur) = every {
        eprintln!(
            "[ridgecode] time trigger: run \"{task}\" every {}s (Ctrl-C to stop; reloads compounding signals each round)",
            dur.as_secs()
        );
    }
    loop {
        // `@path` 引用 → 注入文件正文。继承上个会话的未决信号(信号复利);触发器模式每轮都重载。
        let state = AgentState::new(expand_mentions(task))
            .with_budget(budget)
            .with_signals(load_signal_block());
        match run_streamed(&app, state, &bus).await {
            Ok(out) => {
                let source = trace_and_report(&out);
                agent::fire_session_hooks("stop", &format!("steps={}", out.steps)); // iter-40
                maybe_extract_signals(extractor.as_ref(), &out, &source).await;
            }
            // 触发器(常驻)模式下单轮出错不该掀翻整个循环;一次性模式仍向上抛(非零退出)。
            Err(e) if every.is_some() => {
                agent::fire_session_hooks("stop", "error");
                eprintln!("[ridgecode] error this round: {e}");
            }
            Err(e) => return Err(e),
        }
        match every {
            Some(dur) => tokio::time::sleep(dur).await,
            None => return Ok(()),
        }
    }
}

/// 非 TTY(管道/CI/重定向):无 TUI、无斜杠命令。逐行读 stdin,每行当一个任务串行跑,跨行携带 history。
/// 非交互无法 [y/N] 确认,故一律 [`AutoApprove`](灾难命令仍被 `is_dangerous_command` 硬拦截)。
/// ponytail: headless 恒自动放行;要严格权限门请用 TTY 交互(TUI)。
pub(crate) async fn headless(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    budget: usize,
    mut history: Vec<Message>,
    agents: Arc<agent::Agents>,
    read_only: bool,
) -> anyhow::Result<()> {
    let bus = null_token_bus();
    let extractor = signal_extract_enabled().then(|| provider.clone());
    let app = build_llm_agent_full(
        provider,
        mcp,
        Arc::new(AutoApprove),
        skills,
        bus.clone(),
        agents,
        read_only,
    )?;
    for line in std::io::stdin().lines() {
        let line = line?; // 读到 EOF 迭代自然结束;IO 错误照旧上抛
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        // `@path` 引用 → 注入文件正文;跨行携带 history 实现多轮。
        history.push(Message::user(expand_mentions(input)));
        let state = AgentState::new(input)
            .with_history(history.clone())
            .with_budget(budget)
            .with_signals(load_signal_block());
        match run_streamed(&app, state, &bus).await {
            Ok(out) => {
                history = out.history.clone();
                save_session(&session_path(), &history); // 每轮落盘 → --resume 可恢复
                let source = trace_and_report(&out);
                agent::fire_session_hooks("stop", &format!("steps={}", out.steps)); // iter-40
                maybe_extract_signals(extractor.as_ref(), &out, &source).await;
            }
            Err(e) => {
                agent::fire_session_hooks("stop", "error");
                eprintln!("[ridgecode] error: {e}");
            }
        }
    }
    Ok(())
}

/// 把一轮落成标准存储库的一条 run(`.ridge/runs/<id>/` 含 manifest.json + trace.json,best-effort),
/// 打印结果、播报停机原因。每 run 独立目录,审计历史不再互相覆盖。
/// 返回本 run 的 source id(run 目录名),供自动 signal 抽取器复用同一溯源标签。
pub(crate) fn trace_and_report(out: &AgentState) -> String {
    let run_dir = run_artifacts_dir();
    match write_run(out, &run_dir) {
        Ok(()) => eprintln!("[ridgecode] run trace written {}", run_dir.display()),
        Err(e) => eprintln!("[ridgecode] failed to write run trace: {e}"),
    }
    let source = run_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("run")
        .to_string();
    let reason = halt_reason(out);
    if !reason.is_success() {
        // 响亮失败:护栏熔断/未验证时明确播报,别让「悄悄停」被当成成功(loop engineering:fail loudly)。
        eprintln!(
            "[ridgecode] halt reason: {} (did not pass deterministic verification)",
            reason.as_str()
        );
        // 自动产者:失败落 failure 信号(preserve mistakes),下个会话/下一轮触发自动继承。source=本 run id。
        if let Some(id) = auto_signal_from_run(out, agent::SIGNALS_DIR, &source) {
            eprintln!("[ridgecode] recorded failure signal {id} (next session inherits it)");
        }
    }
    print_report(out);
    source
}

/// 自动 signal 抽取器(opt-in,复利环产者的「发现/待办」侧):run 收尾用 provider 一次性把轨迹
/// 提炼成可复用信号,喂 `.ridge/signals`。best-effort —— 失败/无所得静默,绝不掀翻主流程。
pub(crate) async fn maybe_extract_signals(
    extractor: Option<&Arc<dyn LlmProvider>>,
    out: &AgentState,
    source: &str,
) {
    let Some(p) = extractor else { return };
    let ids = extract_signals_from_run(p.as_ref(), out, agent::SIGNALS_DIR, source).await;
    if !ids.is_empty() {
        eprintln!(
            "[ridgecode] extracted {} compounding signal(s) (next session inherits): {}",
            ids.len(),
            ids.join(", ")
        );
    }
}

/// 本次 run 的留痕目录:`.ridge/runs/<纳秒时间戳>`(cwd 本地,像 `.git` 随项目走)。
/// ponytail: 纳秒时间戳做 id,顺序 CLI 调用间实际不会撞;要严格唯一再引 uuid。
pub(crate) fn run_artifacts_dir() -> std::path::PathBuf {
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::path::Path::new(".ridge")
        .join("runs")
        .join(id.to_string())
}

/// 跑 agent 并**实时把内容渲染到终端**:等待时转 spinner,每个超步一合并就把新产生的
/// 推理 / 工具调用 / 结果 / 校验**彩色打出来** —— 让用户直接在 shell 里看到输出,而非去翻 trace.json。
/// 非 TTY(管道/重定向)时不转 spinner,只顺序输出内容。
pub(crate) async fn run_streamed(
    app: &CompiledGraph<AgentState>,
    state: AgentState,
    token_bus: &TokenBus,
) -> anyhow::Result<AgentState> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    // 逐字流式:注册一个 token sender 到总线;reason 节点边收边发,printer 边收边显。
    let (ttx, mut trx) = tokio::sync::mpsc::unbounded_channel::<provider::StreamChunk>();
    *token_bus.lock().unwrap() = Some(ttx);
    let tty = std::io::stderr().is_terminal();
    let printer = tokio::spawn(async move {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = RichOutput::new().with_color(Color::BrightBlue);
        let dim = RichOutput::new().with_color(Color::Cyan);
        let answer = RichOutput::new().with_color(Color::BrightWhite).bold();
        let think = RichOutput::new().with_color(Color::BrightBlack); // 思考:灰显,不抢眼
        let mut frame = 0usize;
        let mut printed = 0usize; // 已打印到第几条 message
        let mut status = String::from("reasoning");
        let mut streaming = false; // 本超步是否正在逐字流式(流式期间不转 spinner、末尾不重复打)
        let mut stream_mode: Option<bool> = None; // Some(true)=回答段, Some(false)=思考段
        let mut last_todos: Vec<Todo> = Vec::new(); // 任务清单变了才重渲染
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(90));
        loop {
            tokio::select! {
                _ = ticker.tick(), if tty && !streaming => {
                    frame = (frame + 1) % FRAMES.len();
                    eprint!("\r\x1b[K{} {}", spin.format(FRAMES[frame]), dim.format(&status));
                    std::io::stderr().flush().ok();
                }
                Some(chunk) = trx.recv() => {
                    // 分道:回答(🤖 白)恒显,思考(💭 灰)灰显;两段切换时换行 + 换标记。
                    let (is_answer, text) = match &chunk {
                        provider::StreamChunk::Answer(t) => (true, t),
                        provider::StreamChunk::Reasoning(t) => (false, t),
                    };
                    if stream_mode != Some(is_answer) {
                        if stream_mode.is_none() {
                            if tty { eprint!("\r\x1b[K"); } // 清 spinner,起头
                        } else {
                            eprintln!(); // 思考↔回答切换 → 换行分隔
                        }
                        eprint!("{}", if is_answer { answer.format("🤖 ") } else { think.format("💭 ") });
                        stream_mode = Some(is_answer);
                    }
                    eprint!("{}", if is_answer { answer.format(text) } else { think.format(text) });
                    std::io::stderr().flush().ok();
                    streaming = true;
                }
                ev = rx.recv() => match ev {
                    Some(StreamEvent::NodeFinished { node, .. }) => status = node_label(&node),
                    Some(StreamEvent::Superstep { state, .. }) => {
                        if streaming {
                            eprintln!(); // 闭合逐字流式行
                        }
                        for m in state.messages.iter().skip(printed) {
                            // 已逐字流过的最终答案不再整段重打(思考是瞬态,不入 message、天然不重打)。
                            if streaming && m.contains("(final) ") {
                                continue;
                            }
                            if tty { eprint!("\r\x1b[K"); }
                            eprintln!("{}", format_event(m));
                        }
                        printed = state.messages.len();
                        streaming = false; // 超步收尾 → 下个超步 spinner 恢复
                        stream_mode = None;
                        // 任务清单有变化 → 渲染 [x]/[~]/[ ] 给用户看进度。
                        if state.todos != last_todos {
                            if !state.todos.is_empty() {
                                eprintln!("{}", render_todos(&state.todos));
                            }
                            last_todos = state.todos.clone();
                        }
                    }
                    None => break,
                }
            }
        }
        if tty {
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();
        }
    });

    let out = app
        .invoke_with(state, &agent_run_config(), None, Some(&tx))
        .await?;
    drop(tx);
    *token_bus.lock().unwrap() = None; // 关闭 token 通道 → printer 收尾
    let _ = printer.await;
    Ok(out)
}

/// spinner 旁边显示的当前阶段。
pub(crate) fn node_label(node: &str) -> String {
    match node {
        "reason" => "reasoning",
        "act" => "running tools",
        "verify" => "verifying",
        "wrapup" => "wrapping up",
        other => other,
    }
    .to_string()
}

/// 把一条内部事件 message 渲染成彩色终端行(按前缀分类上色)。
pub(crate) fn format_event(m: &str) -> String {
    let ro = |c: Color| RichOutput::new().with_color(c);
    if let Some((_, ans)) = m.split_once("(final) ") {
        // 模型的最终回答 —— 高亮加粗,最显眼。
        return ro(Color::BrightWhite)
            .bold()
            .format(&format!("\n🤖 {ans}\n"));
    }
    if m.starts_with("reason#") {
        // 推理 / 发起工具调用 —— 暗色旁白。
        let body = m.split_once(": ").map_or(m, |x| x.1);
        return ro(Color::Cyan).format(&format!("  ⋯ {body}"));
    }
    if let Some(rest) = m.strip_prefix("act: ") {
        return ro(Color::Yellow).format(&format!("  ▸ {}", truncate(rest, 500)));
    }
    if m.starts_with("verify: PASS") {
        return ro(Color::Green).bold().format(&format!("  ✓ {m}"));
    }
    if m.starts_with("verify: FAIL") {
        return ro(Color::Red).format(&format!("  ✗ {m}"));
    }
    ro(Color::White).format(m)
}

/// 按字符截断长文本(工具输出可能很长,别刷屏)。
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}… (truncated)")
    }
}

/// 离线 demo:脚本大脑 + 假工具,零联网跑通闭环。
pub(crate) async fn run_demo() -> anyhow::Result<()> {
    let app = build_agent(scripted(), default_tool())?;
    let out = app
        .invoke(AgentState::new("make the test suite pass"))
        .await?;
    if let Some(last) = out.messages.last() {
        println!("\n{last}");
    }
    print_report(&out);
    Ok(())
}

pub(crate) fn print_report(out: &AgentState) {
    let status = if out.approved {
        RichOutput::new()
            .with_color(Color::Green)
            .bold()
            .format("✓ approved")
    } else {
        // 显停机原因:让「为何停」一眼可见(budget_exceeded/step_cap/no_progress/…),不再只见「✗」。
        RichOutput::new()
            .with_color(Color::Red)
            .bold()
            .format(&format!("✗ not approved ({})", halt_reason(out).as_str()))
    };
    let stats = RichOutput::new()
        .with_color(Color::Cyan)
        .format(&format!("steps={} tokens={}", out.steps, out.total_tokens));
    println!("\n{status}  {stats}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_event_colorizes_by_kind() {
        let fin = format_event("reason#2: (final) 你好世界");
        assert!(fin.contains("你好世界") && fin.contains("🤖") && fin.contains("\x1b[0m"));
        assert!(format_event("act: web_search -> ok").contains("\x1b[33m")); // 黄
        assert!(format_event("verify: PASS (deterministic gate)").contains("\x1b[32m"));
        // 绿
    }
    #[test]
    fn truncate_caps_long_text() {
        assert_eq!(truncate("abc", 10), "abc");
        assert!(truncate(&"x".repeat(50), 10).ends_with("… (truncated)"));
    }
}
