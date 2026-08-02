use super::*;

pub(crate) struct TuiApprover {
    pub(crate) tx: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
}
impl Approver for TuiApprover {
    fn approve(&self, action: &str, detail: &str) -> bool {
        let (reply, wait) = mpsc::sync_channel(1);
        self.tx
            .send(ApprovalRequest {
                action: action.into(),
                detail: detail.into(),
                reply,
            })
            .is_ok()
            && wait.recv().unwrap_or(false)
    }
}

pub(crate) enum CommitBlock {
    Text {
        text: String,
        color: Color,
    },
    Markdown {
        text: String,
    },
    Reasoning {
        text: String,
        step: usize,
        elapsed_s: u64,
        tokens: usize,
    },
    Tool(ToolBlock),
}

#[derive(Default)]
pub(crate) struct Ui {
    pub(crate) input: InputState,
    /// 补全浮窗(iter-27):Some = 浮窗开(键位模态优先级:审批 > 浮窗 > 输入)。
    pub(crate) popup: Option<Popup>,
    /// 待静态提交队列(iter-26):`note` 只入队,主环 drain 经 `insert_before` 写进终端原生历史。
    /// 队列是瞬态的(每圈清空),无需环形上限 —— 有界性由「提交即出队」保证。
    pub(crate) commits: Vec<CommitBlock>,
    /// 有界实时 transcript：回答、思考与工具块由呈现层统一投影。
    pub(crate) transcript: LiveTranscript,
    /// 已提交工具的有界检视索引；原生 scrollback 保持不可变，详情在此按需展开。
    pub(crate) tool_history: VecDeque<ToolBlock>,
    pub(crate) todos: Vec<Todo>,
    pub(crate) scroll: u16,
    pub(crate) busy: bool,
    pub(crate) phase: String,
    pub(crate) frame: usize,
    /// 启动帧序列进度(iter-28):< SPLASH_TICKS 时 tick 驱动渐显,末帧 banner 入历史。
    pub(crate) splash: usize,
    /// 本任务流式 token 估算累计(iter-31):token_rx 每块 `est_tokens` 累加,Submit 清零、done 保留展示。
    pub(crate) stream_tokens: usize,
    /// 最近收到的真实图超步；未收到同步点时为 0，不把本地帧数冒充执行步数。
    pub(crate) superstep: usize,
    pub(crate) pending_call: Option<provider::ToolCall>,
    /// 排队待跑的提交(iter-33):busy 时 Enter 入队,任务 done 后自动取队首接跑;中断清空。
    pub(crate) queued: VecDeque<String>,
    /// 交互页(iter-35):Some = 模态页开(键位模态优先级:审批 > Panel > 浮窗 > 输入)。
    pub(crate) panel: Option<Panel>,
    /// 自定义/skill 命令请求「以任务身份跑」(iter-39):`run_command` 展开 body 置此,主环取走起任务。
    pub(crate) run_task: Option<String>,
}
impl Ui {
    pub(crate) fn note(&mut self, text: impl Into<String>, color: Color) {
        self.commits.push(CommitBlock::Text {
            text: text.into(),
            color,
        });
    }
    pub(crate) fn note_markdown(&mut self, text: impl Into<String>) {
        self.commits
            .push(CommitBlock::Markdown { text: text.into() });
    }
    #[cfg(test)]
    pub(crate) fn drain_commits(&mut self) -> Vec<(String, Color)> {
        self.drain_commit_blocks()
            .into_iter()
            .flat_map(|block| match block {
                CommitBlock::Text { text, color } => vec![(text, color)],
                CommitBlock::Markdown { text } => vec![(text, Color::White)],
                CommitBlock::Reasoning {
                    text,
                    step,
                    elapsed_s,
                    tokens,
                } => {
                    vec![(
                        format!("{}{text}", fmt_reasoning_meta(step, elapsed_s, tokens)),
                        role_color(Role::Muted),
                    )]
                }
                CommitBlock::Tool(tool) => tool.commit_lines(),
            })
            .collect()
    }
    pub(crate) fn drain_commit_blocks(&mut self) -> Vec<CommitBlock> {
        std::mem::take(&mut self.commits)
    }
    /// 清空逐字流式两路尾巴(回答 + 思考)—— 超步收尾/新任务/中断时用。
    pub(crate) fn clear_streams(&mut self) {
        self.transcript.clear_streams();
    }
    /// 一段流式增量按类别入对应尾巴(回答→白,思考→灰),并计入本任务 token 估算。
    pub(crate) fn push_chunk(&mut self, chunk: provider::StreamChunk) {
        let evicted = match chunk {
            provider::StreamChunk::Answer(t) => {
                self.stream_tokens += est_tokens(&t);
                self.transcript.push_answer(&t)
            }
            provider::StreamChunk::Reasoning(t) => {
                self.stream_tokens += est_tokens(&t);
                self.transcript.push_reasoning(&t)
            }
        };
        self.archive_tools(evicted);
    }
    pub(crate) fn push_tool(&mut self, tool: ToolBlock) {
        let evicted = self.transcript.push_tool(tool);
        self.archive_tools(evicted);
    }
    pub(crate) fn commit_live_tools(&mut self) {
        let tools = self.transcript.drain_tools();
        self.archive_tools(tools);
    }
    /// 把实际收到的 reasoning_content 提交进原生历史；Answer 仍由最终消息的 Markdown 路径提交。
    pub(crate) fn commit_live_reasoning(&mut self, step: usize, elapsed_s: u64) {
        let tokens = self.stream_tokens;
        for text in self.transcript.drain_reasoning() {
            if !text.is_empty() {
                self.commits.push(CommitBlock::Reasoning {
                    text,
                    step,
                    elapsed_s,
                    tokens,
                });
            }
        }
    }
    pub(crate) fn toggle_details(&mut self) -> bool {
        self.transcript.toggle_details()
    }
    pub(crate) fn toggle_details_or_history(&mut self) -> bool {
        if self.has_live_tools() {
            let _ = self.toggle_details();
            true
        } else {
            self.open_tool_history()
        }
    }
    pub(crate) fn toggle_reasoning(&mut self) -> bool {
        self.transcript.toggle_reasoning()
    }
    pub(crate) fn has_live_tools(&self) -> bool {
        self.transcript.has_tools()
    }
    /// 审批回执已送回仍存活的任务：恢复 busy，避免回执到下一事件前误入 Submit。
    pub(crate) fn resume_after_approval(&mut self) {
        self.busy = true;
    }
    pub(crate) fn move_tool_focus(&mut self, delta: i8) -> bool {
        self.transcript.move_tool_focus(delta)
    }

    pub(crate) fn scroll_tool_details(&mut self, delta: i8) -> bool {
        self.transcript.scroll_tool_details(delta)
    }

    pub(crate) fn has_scrollable_live_tool(&self) -> bool {
        self.transcript.has_scrollable_tool_details()
    }

    pub(crate) fn open_tool_history(&mut self) -> bool {
        if self.tool_history.is_empty() {
            return false;
        }
        self.panel = Some(tool_history_panel(&self.tool_history));
        true
    }

    fn archive_tools(&mut self, tools: Vec<ToolBlock>) {
        for tool in tools {
            if self.tool_history.len() == MAX_TOOL_HISTORY {
                self.tool_history.pop_front();
            }
            self.tool_history.push_back(tool.clone());
            self.commits.push(CommitBlock::Tool(tool));
        }
    }
}

/// 把积压的历史行静态提交进终端 scrollback(iter-26 核心):行一经 `insert_before`
/// 即成原生历史,永不参与后续帧的差分重绘 —— Live 视口恒小,闪烁根因根除。
pub(crate) fn flush_commits<B: Backend>(terminal: &mut Terminal<B>, ui: &mut Ui) -> io::Result<()> {
    let width = terminal.size()?.width;
    for block in ui.drain_commit_blocks() {
        match block {
            CommitBlock::Text { text, color } => {
                insert_commit(terminal, text, color, false, Modifier::empty(), width)?;
            }
            CommitBlock::Markdown { text } => {
                insert_commit(terminal, text, Color::White, true, Modifier::empty(), width)?;
            }
            CommitBlock::Reasoning {
                text,
                step,
                elapsed_s,
                tokens,
            } => {
                insert_commit(
                    terminal,
                    format!("{}{text}", fmt_reasoning_meta(step, elapsed_s, tokens)),
                    role_color(Role::Muted),
                    false,
                    Modifier::DIM | Modifier::ITALIC,
                    width,
                )?;
            }
            CommitBlock::Tool(tool) => {
                insert_colored_commit(terminal, tool.commit_lines(), width)?;
            }
        }
    }
    Ok(())
}

fn insert_commit<B: Backend>(
    terminal: &mut Terminal<B>,
    text: String,
    color: Color,
    markdown: bool,
    modifier: Modifier,
    width: u16,
) -> io::Result<()> {
    let text = sanitize_display_text(&text);
    let text = fold_lines(&text, FOLD_MAX); // 呈现层折叠(iter-28):历史不刷屏
                                            // 块间前置一空白行(iter-31 需求 5):连续输出块视觉分栏,不再贴成一片。
    let h = commit_height(&text, width) + 1;
    terminal.insert_before(h, |buf| {
        let mut lines: Vec<Line> = vec![Line::default()];
        if markdown {
            lines.extend(markdown_lines(&text));
        } else {
            lines.extend(text.lines().map(|l| {
                Line::from(Span::styled(
                    l.to_owned(),
                    Style::default().fg(color).add_modifier(modifier),
                ))
            }));
        }
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render(buf.area, buf);
    })
}

fn insert_colored_commit<B: Backend>(
    terminal: &mut Terminal<B>,
    entries: Vec<(String, Color)>,
    width: u16,
) -> io::Result<()> {
    let rows = entries
        .into_iter()
        .flat_map(|(text, color)| {
            let text = fold_lines(&sanitize_display_text(&text), FOLD_MAX);
            text.lines()
                .map(move |line| (line.to_owned(), color))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let logical = rows
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let h = commit_height(&logical, width) + 1;
    terminal.insert_before(h, |buf| {
        let mut lines = vec![Line::default()];
        lines.extend(
            rows.into_iter()
                .map(|(text, color)| Line::from(Span::styled(text, Style::default().fg(color)))),
        );
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render(buf.area, buf);
    })
}
