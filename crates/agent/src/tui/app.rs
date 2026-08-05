use super::*;
use unicode_segmentation::UnicodeSegmentation;

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
        id: PresentationId,
        text: String,
    },
    Reasoning {
        id: PresentationId,
        text: String,
        step: usize,
        elapsed_s: u64,
        tokens: usize,
    },
    Activity {
        sequence: u64,
        kind: ActivityKind,
        text: String,
    },
    Tool(ToolBlock),
}

pub(crate) const MAX_REASONING_HISTORY: usize = 8;
pub(crate) const MAX_REASONING_HISTORY_CHARS: usize = 8_192;

#[derive(Clone, Debug)]
pub(crate) struct ReasoningEntry {
    pub(crate) id: PresentationId,
    pub(crate) step: usize,
    pub(crate) elapsed_s: u64,
    pub(crate) tokens: usize,
    pub(crate) text: String,
}

pub(crate) const MAX_ANSWER_HISTORY: usize = 8;
pub(crate) const MAX_ANSWER_HISTORY_CHARS: usize = 8_192;

#[derive(Clone, Debug)]
pub(crate) struct AnswerEntry {
    pub(crate) id: PresentationId,
    pub(crate) step: usize,
    pub(crate) elapsed_s: u64,
    pub(crate) tokens: usize,
    pub(crate) partial: bool,
    pub(crate) text: String,
}

fn bound_history_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let half = max_chars / 2;
    let head = text.chars().take(half).collect::<String>();
    let tail = text
        .chars()
        .rev()
        .take(half)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head}\n… [{count} chars; middle omitted]\n{tail}")
}

fn bound_answer_history_text(text: &str) -> String {
    bound_history_text(text, MAX_ANSWER_HISTORY_CHARS)
}

fn bound_reasoning_history_text(text: &str) -> String {
    bound_history_text(text, MAX_REASONING_HISTORY_CHARS)
}

pub(crate) const MAX_ACTIVITY_HISTORY: usize = 12;

/// Presentation-only activity category. It is derived from observed UI
/// transitions and never participates in execution or routing decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    System,
    Plan,
    Reasoning,
    Answer,
    Tool,
    Verification,
    Conclusion,
    Waiting,
    Approval,
    Queue,
    Takeover,
    Completed,
    Error,
}

impl ActivityKind {
    /// Compact ASCII tags stay legible in narrow terminals and remain useful
    /// when Nerd Fonts or emoji glyphs are unavailable.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::System => "SYS",
            Self::Plan => "PLAN",
            Self::Reasoning => "THK",
            Self::Answer => "ANS",
            Self::Tool => "TLS",
            Self::Verification => "CHK",
            Self::Conclusion => "SUM",
            Self::Waiting => "WAIT",
            Self::Approval => "ASK",
            Self::Queue => "QUE",
            Self::Takeover => "TAKE",
            Self::Completed => "DONE",
            Self::Error => "ERR",
        }
    }

    /// Keep user-actionable boundaries visible when transient node chatter
    /// fills the bounded audit ledger. This is presentation-only; the
    /// chronological sequence remains available on every retained row.
    fn is_retained_signal(self) -> bool {
        matches!(
            self,
            Self::Plan
                | Self::Verification
                | Self::Waiting
                | Self::Approval
                | Self::Conclusion
                | Self::Takeover
                | Self::Completed
                | Self::Error
        )
    }

    /// Existing activity text remains the single source of truth for the
    /// displayed transition; this classifier only adds presentation metadata.
    fn from_text(text: &str) -> Self {
        let lower = text.to_ascii_lowercase();
        if lower.contains("approval") || text.contains("审批") || text.contains("授权") {
            Self::Approval
        } else if lower.contains("waiting") || text.contains("等待") {
            Self::Waiting
        } else if lower.contains("takeover") || text.contains("接管") {
            Self::Takeover
        } else if lower.starts_with("queued")
            || lower.starts_with("front-queued")
            || text.starts_with("排队")
            || text.starts_with("队列")
        {
            Self::Queue
        } else if lower.contains("error")
            || lower.contains("failed")
            || lower.contains("failure")
            || lower.contains("not approved")
            || text.contains("错误")
            || text.contains("失败")
        {
            Self::Error
        } else if lower.contains("completed")
            || lower.contains("approved")
            || lower.contains("done")
            || text.contains("完成")
        {
            Self::Completed
        } else if lower.contains("settling")
            || lower.contains("conclusion")
            || lower.contains("synthes")
            || lower.contains("wrapping up")
            || text.contains("结论")
            || text.contains("总结")
            || text.contains("收敛")
        {
            Self::Conclusion
        } else if lower.contains("verify")
            || lower.contains("checker")
            || text.contains("验证")
            || text.contains("检查")
        {
            Self::Verification
        } else if lower.contains("thinking")
            || lower.contains("reasoning")
            || lower.contains("investigat")
            || lower.contains("survey")
            || lower.contains("analysis")
            || lower.contains("reason")
            || text.contains("调查")
            || text.contains("分析")
            || text.contains("推理")
            || text.contains("思考")
        {
            Self::Reasoning
        } else if lower.contains("answering") || text.contains("回答") || text.contains("答复")
        {
            Self::Answer
        } else if lower.contains("running tool")
            || lower.starts_with("tool")
            || text.starts_with("工具")
        {
            Self::Tool
        } else {
            Self::System
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActivityEntry {
    pub(crate) sequence: u64,
    pub(crate) kind: ActivityKind,
    pub(crate) text: String,
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
    /// 已提交 reasoning 的有界检视索引；原生 scrollback 保持不可变，详情在此按需展开。
    pub(crate) reasoning_history: VecDeque<ReasoningEntry>,
    /// Completed and interrupted Answer bodies stay available behind the
    /// bounded scrollback fold; `partial` keeps interruption distinct from a
    /// completed final response.
    pub(crate) answer_history: VecDeque<AnswerEntry>,
    /// 有界语义桥: live、历史与静态投影共用 id/status/计量,不复制正文。
    pub(crate) presentation: PresentationLedger,
    pub(crate) todos: Vec<Todo>,
    pub(crate) scroll: u16,
    pub(crate) busy: bool,
    /// No event/stream chunk arrived for the stale threshold; render an explicit
    /// waiting state instead of leaving the user to infer a permanent reasoning loop.
    pub(crate) waiting: bool,
    pub(crate) phase: String,
    /// 当前实际观测到的 Agent 活动；与 phase 分开，避免把 node 名误当实时进展。
    pub(crate) activity: String,
    /// 最近实际观测到的活动阶段；有界，供 Ctrl+T / `/activity` 检视，不参与执行状态。
    pub(crate) activity_history: VecDeque<ActivityEntry>,
    pub(crate) activity_sequence: u64,
    /// Start time of the currently displayed activity; diagnostics only.
    pub(crate) activity_started: Option<std::time::Instant>,
    pub(crate) frame: usize,
    /// 启动帧序列进度(iter-28):< SPLASH_TICKS 时 tick 驱动渐显,末帧 banner 入历史。
    pub(crate) splash: usize,
    /// 本任务流式 token 估算累计(iter-31):token_rx 每块 `est_tokens` 累加,Submit 清零、done 保留展示。
    pub(crate) stream_tokens: usize,
    /// 当前任务 provider 回传的输入 token 累计；首个同步点前由 ctx 估算兜底。
    pub(crate) input_tokens: usize,
    /// 当前任务 provider 回传的输出 token 累计；回传前由流式文本估算兜底。
    pub(crate) output_tokens: usize,
    /// 最近收到的真实图超步；未收到同步点时为 0，不把本地帧数冒充执行步数。
    pub(crate) superstep: usize,
    pub(crate) pending_call: Option<provider::ToolCall>,
    /// 排队待跑的提交(iter-33):busy 时 Enter 入队,任务 done 后自动取队首接跑;中断清空。
    pub(crate) queued: VecDeque<String>,
    /// 交互页(iter-35):Some = 模态页开(键位模态优先级:审批 > Panel > 浮窗 > 输入)。
    pub(crate) panel: Option<Panel>,
    /// Automatic localhost OAuth callback, polled by the TUI main loop.
    pub(crate) oauth_callback: Option<LocalOAuthCallback>,
    /// Device OAuth fallback, used when the registered localhost ports are unavailable.
    pub(crate) oauth_device: Option<DeviceOAuthFlow>,
    /// Visible device-auth prompt; kept on-screen instead of only in terminal scrollback.
    pub(crate) device_auth_status: Option<String>,
    /// Background-fetched model catalog; `/model` only renders this cache.
    pub(crate) model_catalog: Option<ModelCatalog>,
    /// Set after OAuth changes so the catalog is fetched with the new token.
    pub(crate) model_catalog_reload: bool,
    /// Active ChatGPT/Codex reasoning effort; `None` means provider default.
    pub(crate) effort: Option<String>,
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
    #[cfg(test)]
    pub(crate) fn note_markdown(&mut self, text: impl Into<String>) {
        let step = self.superstep;
        let tokens = self.stream_tokens;
        self.note_markdown_with_meta(text, step, 0, tokens);
    }
    pub(crate) fn note_markdown_with_meta(
        &mut self,
        text: impl Into<String>,
        step: usize,
        elapsed_s: u64,
        tokens: usize,
    ) {
        let text = sanitize_display_text(&text.into());
        if text.is_empty() {
            return;
        }
        let id = self.presentation.allocate(
            PresentationChannel::Answer,
            PresentationMetrics {
                step,
                elapsed_s,
                tokens,
                chars: text.chars().count(),
            },
        );
        self.presentation.settle(
            PresentationChannel::Answer,
            id,
            PresentationStatus::Committed,
            PresentationMetrics {
                step,
                elapsed_s,
                tokens,
                chars: text.chars().count(),
            },
        );
        self.retain_answer_history(id, &text, step, elapsed_s, tokens, false);
        self.commits.push(CommitBlock::Markdown { id, text });
        self.refresh_answer_history_panel();
    }
    fn retain_answer_history(
        &mut self,
        id: PresentationId,
        text: &str,
        step: usize,
        elapsed_s: u64,
        tokens: usize,
        partial: bool,
    ) {
        let history_text = bound_answer_history_text(text);
        if self.answer_history.len() == MAX_ANSWER_HISTORY {
            let evict = self
                .answer_history
                .iter()
                .position(|entry| entry.partial)
                .unwrap_or(0);
            self.answer_history.remove(evict);
        }
        self.answer_history.push_back(AnswerEntry {
            id,
            step,
            elapsed_s,
            tokens,
            partial,
            text: history_text,
        });
    }
    #[cfg(test)]
    pub(crate) fn drain_commits(&mut self) -> Vec<(String, Color)> {
        self.drain_commit_blocks()
            .into_iter()
            .flat_map(|block| match block {
                CommitBlock::Text { text, color } => vec![(text, color)],
                CommitBlock::Markdown { id, text } => {
                    debug_assert!(self.presentation.contains(PresentationChannel::Answer, id));
                    vec![(text, role_color(Role::Answer))]
                }
                CommitBlock::Reasoning {
                    id,
                    text,
                    step,
                    elapsed_s,
                    tokens,
                } => {
                    debug_assert!(self
                        .presentation
                        .contains(PresentationChannel::Reasoning, id));
                    vec![(
                        reasoning_commit_text(&text, step, elapsed_s, tokens),
                        role_color(Role::Reasoning),
                    )]
                }
                CommitBlock::Activity {
                    sequence,
                    kind,
                    text,
                } => vec![(
                    activity_commit_text(sequence, kind, &text),
                    role_color(activity_role(kind)),
                )],
                CommitBlock::Tool(tool) => tool.commit_lines(),
            })
            .collect()
    }
    pub(crate) fn drain_commit_blocks(&mut self) -> Vec<CommitBlock> {
        std::mem::take(&mut self.commits)
    }
    /// 清空逐字流式两路尾巴(回答 + 思考)—— 超步收尾/新任务/中断时用。
    pub(crate) fn clear_streams(&mut self) {
        for row in self.transcript.inspector_rows() {
            let chars = self
                .transcript
                .live_block_chars(row.focus)
                .unwrap_or_else(|| row.detail.chars().count());
            match row.focus {
                LiveBlockFocus::Answer(id) => self.presentation.settle(
                    PresentationChannel::Answer,
                    id,
                    PresentationStatus::Archived,
                    PresentationMetrics {
                        step: self.superstep,
                        elapsed_s: 0,
                        tokens: self.stream_tokens,
                        chars,
                    },
                ),
                LiveBlockFocus::Reasoning(id) => self.presentation.settle(
                    PresentationChannel::Reasoning,
                    id,
                    PresentationStatus::Archived,
                    PresentationMetrics {
                        step: self.superstep,
                        elapsed_s: 0,
                        tokens: self.stream_tokens,
                        chars,
                    },
                ),
                LiveBlockFocus::Tool(_) => {}
            }
        }
        self.transcript.clear_streams();
        // Once real interaction starts, never let the idle splash animation
        // resume after a task finishes or is taken over.
        self.splash = SPLASH_TICKS;
        self.refresh_live_history_panel();
    }
    pub(crate) fn set_activity(&mut self, text: impl Into<String>) {
        let text = sanitize_display_text(&text.into());
        if text.is_empty() || text == self.activity {
            return;
        }
        self.activity = text.clone();
        self.activity_started = Some(std::time::Instant::now());
        self.record_activity(ActivityKind::from_text(&text), text);
    }

    /// Add a non-current event, such as a queued message or a waiting edge.
    /// This keeps the current model phase truthful while exposing intervention
    /// events in the activity timeline.
    pub(crate) fn record_activity(&mut self, kind: ActivityKind, text: impl Into<String>) {
        let text = sanitize_display_text(&text.into());
        if text.is_empty()
            || self
                .activity_history
                .back()
                .is_some_and(|entry| entry.kind == kind && entry.text == text)
        {
            return;
        }
        self.activity_sequence = self.activity_sequence.wrapping_add(1);
        if self.activity_history.len() >= MAX_ACTIVITY_HISTORY {
            let evict = self
                .activity_history
                .iter()
                .position(|entry| !entry.kind.is_retained_signal())
                .unwrap_or(0);
            self.activity_history.remove(evict);
        }
        self.activity_history.push_back(ActivityEntry {
            sequence: self.activity_sequence,
            kind,
            text: text.clone(),
        });
        if kind.is_retained_signal() {
            self.commits.push(CommitBlock::Activity {
                sequence: self.activity_sequence,
                kind,
                text,
            });
        }
        if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::Activity)
        {
            self.refresh_activity_panel();
        }
    }

    /// Keep the model's changing TODO snapshot in the same bounded audit
    /// surface as other actionable boundaries. The execution graph still
    /// owns the TODO values; this is only a sanitized, bounded projection.
    pub(crate) fn record_plan(&mut self, text: impl Into<String>) {
        let text = bound_history_text(&text.into(), MAX_REASONING_HISTORY_CHARS);
        if !text.is_empty() {
            self.record_activity(ActivityKind::Plan, text);
        }
    }

    fn refresh_activity_panel(&mut self) {
        let Some(previous) = self.panel.take() else {
            return;
        };
        if previous.kind != PanelKind::Activity {
            self.panel = Some(previous);
            return;
        }

        let selected = previous
            .detail_open
            .then(|| previous.selected().map(|row| row.value.clone()))
            .flatten();
        let query = previous.query.clone();
        let detail_open = previous.detail_open;
        let detail_scroll = previous.detail_scroll;
        let mut panel = activity_panel(&self.activity_history);
        panel.query = query;
        panel.retype();
        if let Some(value) = selected {
            if let Some(position) = panel
                .view
                .iter()
                .position(|&index| panel.rows[index].value == value)
            {
                panel.sel = position;
            }
        }
        panel.detail_open =
            detail_open && panel.selected().is_some_and(|row| !row.value.is_empty());
        panel.detail_scroll = detail_scroll;
        self.panel = Some(panel);
    }
    /// 一段流式增量按类别入对应尾巴(回答→白,思考→灰),并计入本任务 token 估算。
    pub(crate) fn push_chunk(&mut self, chunk: provider::StreamChunk) {
        let (channel, text) = match chunk {
            provider::StreamChunk::Answer(t) => {
                self.stream_tokens += est_tokens(&t);
                (PresentationChannel::Answer, t)
            }
            provider::StreamChunk::Reasoning(t) => {
                self.stream_tokens += est_tokens(&t);
                (PresentationChannel::Reasoning, t)
            }
        };
        let text = sanitize_display_text(&text);
        if text.is_empty() {
            return;
        }
        let id = self.stream_presentation_id(channel);
        let evicted = match channel {
            PresentationChannel::Answer => self.transcript.push_answer_with_id(&text, id),
            PresentationChannel::Reasoning => self.transcript.push_reasoning_with_id(&text, id),
            PresentationChannel::Tool => Vec::new(),
        };
        let live_channel = match channel {
            PresentationChannel::Answer => LiveChannel::Answer,
            PresentationChannel::Reasoning => LiveChannel::Reasoning,
            PresentationChannel::Tool => LiveChannel::Tool,
        };
        self.presentation.touch(
            channel,
            id,
            PresentationMetrics {
                step: self.superstep,
                elapsed_s: 0,
                tokens: self.stream_tokens,
                chars: self
                    .transcript
                    .current_stream_chars(live_channel)
                    .unwrap_or_else(|| text.chars().count()),
            },
        );
        self.archive_tools(evicted);
        self.reconcile_presentation();
        self.refresh_live_history_panel();
    }
    pub(crate) fn push_tool(&mut self, tool: ToolBlock) {
        let id = self.presentation.allocate(
            PresentationChannel::Tool,
            PresentationMetrics {
                step: self.superstep,
                elapsed_s: 0,
                tokens: self.stream_tokens,
                chars: 0,
            },
        );
        let (evicted, actual_id, created) = self.transcript.push_tool_with_id(tool, id);
        if !created {
            self.presentation.discard(PresentationChannel::Tool, id);
        }
        if let Some(chars) = self.transcript.live_tool_chars(actual_id) {
            self.presentation.touch(
                PresentationChannel::Tool,
                actual_id,
                PresentationMetrics {
                    step: self.superstep,
                    elapsed_s: 0,
                    tokens: self.stream_tokens,
                    chars,
                },
            );
        }
        self.archive_tools(evicted);
        self.reconcile_presentation();
        self.refresh_live_history_panel();
    }
    pub(crate) fn commit_live_tools(&mut self) {
        let tools = self.transcript.drain_tools();
        self.archive_tools(tools);
        self.refresh_live_history_panel();
    }
    /// 把实际收到的 reasoning_content 提交进原生历史；Answer 仍由最终消息的 Markdown 路径提交。
    pub(crate) fn commit_live_reasoning(&mut self, step: usize, elapsed_s: u64) {
        let tokens = self.stream_tokens;
        for (id, text) in self.transcript.drain_reasoning_with_ids() {
            if !text.is_empty() {
                let history_text = bound_reasoning_history_text(&text);
                if self.reasoning_history.len() == MAX_REASONING_HISTORY {
                    self.reasoning_history.pop_front();
                }
                self.reasoning_history.push_back(ReasoningEntry {
                    id,
                    step,
                    elapsed_s,
                    tokens,
                    text: history_text,
                });
                self.presentation.settle(
                    PresentationChannel::Reasoning,
                    id,
                    PresentationStatus::Committed,
                    PresentationMetrics {
                        step,
                        elapsed_s,
                        tokens,
                        chars: text.chars().count(),
                    },
                );
                self.commits.push(CommitBlock::Reasoning {
                    id,
                    text,
                    step,
                    elapsed_s,
                    tokens,
                });
            }
        }
        self.refresh_reasoning_history_panel();
        self.refresh_live_history_panel();
    }
    /// Retain a streamed Answer when interruption/error prevents the graph
    /// from producing its `(final)` event. It enters the same bounded archive
    /// as completed answers with `partial = true`; the explicit note and row
    /// marker prevent the static Markdown rail from implying completion.
    pub(crate) fn commit_live_answers(&mut self, reason: &str, step: usize, elapsed_s: u64) {
        let answers = self
            .transcript
            .drain_answers_with_ids()
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .collect::<Vec<_>>();
        if answers.is_empty() {
            self.refresh_live_history_panel();
            return;
        }
        let tokens = self.stream_tokens;
        for (id, text) in &answers {
            self.retain_answer_history(*id, text, step, elapsed_s, tokens, true);
            self.presentation.settle(
                PresentationChannel::Answer,
                *id,
                PresentationStatus::Partial,
                PresentationMetrics {
                    step,
                    elapsed_s,
                    tokens,
                    chars: text.chars().count(),
                },
            );
        }
        self.note(format!("partial answer retained · {reason}"), Color::Yellow);
        self.commits.extend(
            answers
                .into_iter()
                .map(|(id, text)| CommitBlock::Markdown { id, text }),
        );
        self.refresh_answer_history_panel();
        self.refresh_live_history_panel();
    }
    pub(crate) fn toggle_details(&mut self) -> bool {
        self.transcript.toggle_details()
    }
    pub(crate) fn toggle_details_or_history(&mut self) -> bool {
        if self.has_live_tools() {
            // A global audit shortcut is an attention switch: close any
            // covering panel so the changed live tool projection is visible.
            let _ = self.toggle_details();
            self.panel = None;
            true
        } else if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::ToolHistory)
        {
            self.panel = None;
            true
        } else {
            self.open_tool_history()
        }
    }
    pub(crate) fn toggle_reasoning(&mut self) -> bool {
        self.transcript.toggle_reasoning()
    }
    pub(crate) fn toggle_focused_semantic(&mut self) -> bool {
        self.transcript.toggle_focused_semantic()
    }
    pub(crate) fn toggle_reasoning_or_history(&mut self) -> bool {
        if self.transcript.has_reasoning() {
            let _ = self.toggle_reasoning();
            self.panel = None;
            true
        } else if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::ReasoningHistory)
        {
            self.panel = None;
            true
        } else {
            self.open_reasoning_history()
        }
    }
    pub(crate) fn toggle_answer_or_history(&mut self) -> bool {
        if self.busy && self.transcript.has_answer() {
            let focused_answer = matches!(
                self.transcript.focused_block(),
                Some(LiveBlockFocus::Answer(_))
            );
            if focused_answer && self.transcript.is_inspecting() {
                self.transcript.follow_live();
            } else {
                self.transcript.focus_latest(LiveChannel::Answer);
            }
            self.panel = None;
            return true;
        }
        if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::AnswerHistory)
        {
            self.panel = None;
            true
        } else {
            self.open_answer_history()
        }
    }
    pub(crate) fn scroll_live(&mut self, delta: i8) -> bool {
        self.transcript.scroll_live(delta)
    }
    pub(crate) fn scroll_live_page(&mut self, direction: i8, page_rows: usize) -> bool {
        self.transcript.scroll_live_page(direction, page_rows)
    }
    pub(crate) fn follow_live(&mut self) -> bool {
        self.transcript.follow_live()
    }
    pub(crate) fn hold_live(&mut self) -> bool {
        self.transcript.hold_live()
    }
    pub(crate) fn has_inspectable_live_output(&self) -> bool {
        self.transcript.has_inspectable_output()
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

    pub(crate) fn move_semantic_focus(&mut self, delta: i8) -> bool {
        self.transcript.move_semantic_focus(delta)
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

    pub(crate) fn open_reasoning_history(&mut self) -> bool {
        if self.reasoning_history.is_empty() {
            return false;
        }
        self.panel = Some(reasoning_history_panel(&self.reasoning_history));
        true
    }

    pub(crate) fn open_answer_history(&mut self) -> bool {
        if self.answer_history.is_empty() {
            return false;
        }
        self.panel = Some(answer_history_panel(&self.answer_history));
        true
    }

    pub(crate) fn open_live_history(&mut self) -> bool {
        if !self.transcript.has_history() {
            return false;
        }
        self.panel = Some(live_history_panel_with_queue(
            &self.transcript,
            &self.queued,
        ));
        self.sync_live_panel_focus();
        true
    }

    /// Open the live audit surface with an optional query without touching the
    /// input buffer or interrupting the running model task.
    pub(crate) fn open_live_search(&mut self, query: &str) -> bool {
        if !self.transcript.has_history() {
            return false;
        }
        let mut panel = live_history_panel_with_queue(&self.transcript, &self.queued);
        panel.query = query.trim().to_owned();
        panel.retype();
        self.panel = Some(panel);
        self.sync_live_panel_focus();
        true
    }

    /// Apply the selected live Inspector row to the render projection.  Panel
    /// navigation is otherwise presentation-only; this small bridge gives a
    /// historical tool a durable focus without stealing Tab from completion.
    pub(crate) fn sync_live_panel_focus(&mut self) {
        let focus = self.panel.as_ref().and_then(|panel| {
            if panel.kind != PanelKind::LiveHistory {
                return None;
            }
            match panel.selected_action() {
                PanelRowAction::FocusLiveBlock(focus) => Some(focus),
                PanelRowAction::RemoveQueued(_) | PanelRowAction::None => None,
            }
        });
        if let Some(focus) = focus {
            self.transcript.focus_live_block(focus);
            // A detail search opens the Inspector preview.  Mirror that
            // intent into the live projection so a folded Tool match is not
            // visible only in the modal while the stream keeps running.
            let detail_open = self
                .panel
                .as_ref()
                .is_some_and(|panel| panel.kind == PanelKind::LiveHistory && panel.detail_open);
            if detail_open {
                if let LiveBlockFocus::Tool(id) = focus {
                    self.transcript.set_tool_expanded(id, true);
                }
            }
        }
    }

    /// Toggle the selected Inspector detail and mirror tool expansion in the
    /// underlying live viewport.  Pending rows and answer/reasoning rows keep
    /// their panel-only detail semantics.
    pub(crate) fn toggle_live_panel_detail(&mut self) -> bool {
        let focus = self.panel.as_ref().and_then(|panel| {
            (panel.kind == PanelKind::LiveHistory).then(|| match panel.selected_action() {
                PanelRowAction::FocusLiveBlock(focus) => Some(focus),
                PanelRowAction::RemoveQueued(_) | PanelRowAction::None => None,
            })
        });
        if let Some(Some(LiveBlockFocus::Tool(id))) = focus {
            self.transcript.focus_live_block(LiveBlockFocus::Tool(id));
            let next = self
                .panel
                .as_ref()
                .map(|panel| !panel.detail_open)
                .unwrap_or(false);
            self.transcript.set_tool_expanded(id, next);
        }
        self.panel
            .as_mut()
            .filter(|panel| panel.kind == PanelKind::LiveHistory)
            .map(|panel| panel.toggle_detail())
            .unwrap_or(false)
    }

    pub(crate) fn toggle_live_history(&mut self) -> bool {
        if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::LiveHistory)
        {
            self.panel = None;
            true
        } else if self
            .panel
            .as_ref()
            .is_none_or(|panel| panel.allows_attention_switch())
        {
            self.open_live_history()
        } else {
            false
        }
    }

    pub(crate) fn refresh_live_history_panel(&mut self) {
        let Some(panel) = self
            .panel
            .as_ref()
            .filter(|panel| panel.kind == PanelKind::LiveHistory)
        else {
            return;
        };
        if !self.transcript.has_history() {
            self.panel = None;
            return;
        }
        let query = panel.query.clone();
        let selected = panel.sel;
        let selected_value = panel.selected().map(|row| row.value.clone());
        let old_len = panel.rows.len();
        let detail_open = panel.detail_open;
        let detail_scroll = panel.detail_scroll;
        let mut refreshed = live_history_panel_with_queue(&self.transcript, &self.queued);
        refreshed.query = query;
        refreshed.retype();
        if let Some(value) = selected_value {
            if let Some(position) = refreshed
                .view
                .iter()
                .position(|&index| refreshed.rows[index].value == value)
            {
                refreshed.sel = position;
            } else {
                let added = refreshed.rows.len().saturating_sub(old_len);
                refreshed.sel = selected
                    .saturating_add(added)
                    .min(refreshed.view.len().saturating_sub(1));
            }
        } else {
            refreshed.sel = selected.min(refreshed.view.len().saturating_sub(1));
        }
        refreshed.detail_open = detail_open;
        refreshed.detail_scroll = detail_scroll;
        self.panel = Some(refreshed);
        self.sync_live_panel_focus();
    }

    fn refresh_reasoning_history_panel(&mut self) {
        let Some(panel) = self
            .panel
            .as_ref()
            .filter(|panel| panel.kind == PanelKind::ReasoningHistory)
        else {
            return;
        };
        let query = panel.query.clone();
        let selected = panel.sel;
        let detail_open = panel.detail_open;
        let detail_scroll = panel.detail_scroll;
        let mut refreshed = reasoning_history_panel(&self.reasoning_history);
        refreshed.query = query;
        refreshed.retype();
        refreshed.sel = selected.min(refreshed.view.len().saturating_sub(1));
        refreshed.detail_open = detail_open;
        refreshed.detail_scroll = detail_scroll;
        self.panel = Some(refreshed);
    }

    fn refresh_answer_history_panel(&mut self) {
        let Some(panel) = self
            .panel
            .as_ref()
            .filter(|panel| panel.kind == PanelKind::AnswerHistory)
        else {
            return;
        };
        let query = panel.query.clone();
        let selected = panel.sel;
        let detail_open = panel.detail_open;
        let detail_scroll = panel.detail_scroll;
        let selected_value = panel.selected().map(|row| row.value.clone());
        let mut refreshed = answer_history_panel(&self.answer_history);
        refreshed.query = query;
        refreshed.retype();
        if let Some(value) = selected_value {
            if let Some(position) = refreshed
                .view
                .iter()
                .position(|&index| refreshed.rows[index].value == value)
            {
                refreshed.sel = position;
            } else {
                refreshed.sel = selected.min(refreshed.view.len().saturating_sub(1));
            }
        } else {
            refreshed.sel = selected.min(refreshed.view.len().saturating_sub(1));
        }
        refreshed.detail_open = detail_open;
        refreshed.detail_scroll = detail_scroll;
        self.panel = Some(refreshed);
    }

    pub(crate) fn open_activity_panel(&mut self) {
        self.panel = Some(activity_panel(&self.activity_history));
    }

    pub(crate) fn open_queue_panel(&mut self) {
        self.panel = Some(queue_panel(&self.queued));
    }

    pub(crate) fn toggle_queue_panel(&mut self) -> bool {
        if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::Queue)
        {
            self.panel = None;
            true
        } else if self
            .panel
            .as_ref()
            .is_none_or(|panel| panel.allows_attention_switch())
        {
            self.open_queue_panel();
            true
        } else {
            false
        }
    }

    pub(crate) fn remove_queued(&mut self, index: usize) -> Option<String> {
        self.queued.remove(index)
    }

    /// Keep an open queue panel truthful while the runner consumes or edits
    /// the FIFO. Preserve the filter and keep selection near the old row.
    pub(crate) fn refresh_queue_panel(&mut self) {
        if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::LiveHistory)
        {
            self.refresh_live_history_panel();
            return;
        }
        let Some(panel) = self
            .panel
            .as_ref()
            .filter(|panel| panel.kind == PanelKind::Queue)
        else {
            return;
        };
        let query = panel.query.clone();
        let selected = panel.sel;
        let mut refreshed = queue_panel(&self.queued);
        refreshed.query = query;
        refreshed.retype();
        refreshed.sel = selected.min(refreshed.view.len().saturating_sub(1));
        self.panel = Some(refreshed);
    }

    pub(crate) fn toggle_activity_panel(&mut self) {
        if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::Activity)
        {
            self.panel = None;
        } else {
            self.open_activity_panel();
        }
    }

    fn archive_tools(&mut self, tools: Vec<ToolBlock>) {
        for tool in tools {
            self.presentation.settle(
                PresentationChannel::Tool,
                tool.presentation_id(),
                PresentationStatus::Committed,
                PresentationMetrics {
                    step: self.superstep,
                    elapsed_s: 0,
                    tokens: self.stream_tokens,
                    chars: tool.presentation_chars(),
                },
            );
            if self.tool_history.len() == MAX_TOOL_HISTORY {
                self.tool_history.pop_front();
            }
            self.tool_history.push_back(tool.clone());
            self.commits.push(CommitBlock::Tool(tool));
        }
    }

    fn stream_presentation_id(&mut self, channel: PresentationChannel) -> PresentationId {
        let live_channel = match channel {
            PresentationChannel::Answer => LiveChannel::Answer,
            PresentationChannel::Reasoning => LiveChannel::Reasoning,
            PresentationChannel::Tool => LiveChannel::Tool,
        };
        if let Some(id) = self.transcript.current_stream_id(live_channel) {
            if self.presentation.contains(channel, id) {
                return id;
            }
        }
        self.presentation.allocate(
            channel,
            PresentationMetrics {
                step: self.superstep,
                elapsed_s: 0,
                tokens: self.stream_tokens,
                chars: 0,
            },
        )
    }

    fn reconcile_presentation(&mut self) {
        let live = self
            .transcript
            .inspector_rows()
            .into_iter()
            .map(|row| match row.focus {
                LiveBlockFocus::Answer(id) => (PresentationChannel::Answer, id),
                LiveBlockFocus::Reasoning(id) => (PresentationChannel::Reasoning, id),
                LiveBlockFocus::Tool(id) => (PresentationChannel::Tool, id),
            })
            .collect::<std::collections::HashSet<_>>();
        let stale = self
            .presentation
            .records()
            .iter()
            .filter(|record| record.status == PresentationStatus::Live)
            .filter(|record| !live.contains(&(record.channel, record.id)))
            .map(|record| (record.channel, record.id))
            .collect::<Vec<_>>();
        for (channel, id) in stale {
            self.presentation.archive(channel, id);
        }
    }
}

/// 把积压的历史行静态提交进终端 scrollback(iter-26 核心):行一经 `insert_before`
/// 即成原生历史,永不参与后续帧的差分重绘 —— Live 视口恒小,闪烁根因根除。
pub(crate) fn apply_paste(ui: &mut Ui, text: &str) {
    ui.popup = None;
    ui.input.insert_str(text);
}

pub(crate) fn flush_commits<B: Backend>(terminal: &mut Terminal<B>, ui: &mut Ui) -> io::Result<()> {
    let width = terminal.size()?.width;
    let mut lines = Vec::new();
    for block in ui.drain_commit_blocks() {
        match block {
            CommitBlock::Text { text, color } => {
                lines.extend(commit_lines(
                    text,
                    color,
                    false,
                    false,
                    Modifier::empty(),
                    width,
                ));
            }
            CommitBlock::Markdown { id, text } => {
                debug_assert!(ui.presentation.contains(PresentationChannel::Answer, id));
                let partial = ui.presentation.status(PresentationChannel::Answer, id)
                    == Some(PresentationStatus::Partial);
                let metrics = ui.presentation.metrics(PresentationChannel::Answer, id);
                lines.extend(commit_lines_with_answer_metrics(
                    text,
                    role_color(Role::Answer),
                    true,
                    partial,
                    Modifier::empty(),
                    width,
                    metrics,
                ));
            }
            CommitBlock::Reasoning {
                id,
                text,
                step,
                elapsed_s,
                tokens,
            } => {
                debug_assert!(ui.presentation.contains(PresentationChannel::Reasoning, id));
                lines.extend(reasoning_commit_lines(
                    &text, step, elapsed_s, tokens, width,
                ));
            }
            CommitBlock::Activity {
                sequence,
                kind,
                text,
            } => {
                lines.extend(activity_commit_lines(sequence, kind, &text, width));
            }
            CommitBlock::Tool(tool) => {
                lines.extend(colored_commit_lines(static_tool_lines(&tool, width), width));
            }
        }
    }
    insert_bounded_commit_lines(terminal, lines)
}

pub(crate) fn reasoning_commit_lines(
    text: &str,
    step: usize,
    elapsed_s: u64,
    tokens: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let text = fold_lines(
        &sanitize_display_text(&bound_reasoning_history_text(text)),
        FOLD_MAX,
    );
    let meta = fmt_reasoning_meta(step, elapsed_s, tokens);
    let base = Style::default()
        .fg(role_color(Role::Reasoning))
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    let meta_style = Style::default()
        .fg(role_color(Role::Label))
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    let hint_style = Style::default()
        .fg(role_color(Role::Muted))
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    let mut in_code = false;
    let mut alert_role = None;
    let source_lines = text.lines().collect::<Vec<_>>();
    let edges = alert_edges(source_lines.iter().copied());
    let mut lines = vec![Line::default()];

    for (index, line) in source_lines.into_iter().enumerate() {
        let (mut body, next_code) = md_line_spans_with_alert(line, in_code, &mut alert_role);
        in_code = next_code;
        if let Some(edge) = edges.get(index).copied().flatten() {
            apply_alert_edge(&mut body, edge);
        }
        for span in &mut body {
            span.style = base.patch(span.style);
        }

        let mut spans = Vec::with_capacity(body.len() + 4);
        spans.push(Span::styled(if index == 0 { "┊ " } else { "│ " }, base));
        if index == 0 {
            spans.push(Span::styled(meta.clone(), meta_style));
        }
        spans.extend(body);
        if index == 0 {
            spans.push(Span::styled("  [Ctrl+R history]", hint_style));
        }
        lines.push(Line::from(spans));
    }

    if lines.len() == 1 {
        lines.push(Line::from(vec![
            Span::styled("┊ ", base),
            Span::styled(meta, meta_style),
        ]));
    }

    wrap_commit_lines(lines, width)
}

#[cfg(test)]
fn reasoning_commit_text(text: &str, step: usize, elapsed_s: u64, tokens: usize) -> String {
    let text = bound_reasoning_history_text(text);
    let meta = fmt_reasoning_meta(step, elapsed_s, tokens);
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return format!("┊ {meta}");
    };
    let mut committed = format!("┊ {meta}{first}  [Ctrl+R history]");
    for line in lines {
        committed.push('\n');
        committed.push_str("│ ");
        committed.push_str(line);
    }
    committed
}

#[cfg(test)]
fn activity_commit_text(sequence: u64, kind: ActivityKind, text: &str) -> String {
    format!("⟦{} #{sequence}⟧ {text}  [Ctrl+T activity]", kind.tag())
}

fn activity_role(kind: ActivityKind) -> Role {
    match kind {
        ActivityKind::Plan => Role::Reasoning,
        ActivityKind::Waiting | ActivityKind::Approval => Role::Warn,
        ActivityKind::Takeover => Role::Primary,
        ActivityKind::Completed | ActivityKind::Conclusion => Role::Success,
        ActivityKind::Error => Role::Error,
        _ => Role::Info,
    }
}

/// Static activity anchors use a small semantic hierarchy: the lifecycle tag
/// is the scan target, the observed text stays readable, and the recovery
/// shortcut remains deliberately quiet. Text, wrapping, and commit height
/// stay identical to the plain activity projection.
fn activity_commit_lines(
    sequence: u64,
    kind: ActivityKind,
    text: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let text = fold_lines(&sanitize_display_text(text), FOLD_MAX);
    let prefix = format!("⟦{} #{sequence}⟧ ", kind.tag());
    let hint = "  [Ctrl+T activity]";
    let first = text.lines().next().unwrap_or("");
    let role = activity_role(kind);
    let tag_style = Style::default()
        .fg(role_color(role))
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default()
        .fg(role_color(role))
        .add_modifier(Modifier::DIM);
    let hint_style = Style::default()
        .fg(role_color(Role::Muted))
        .add_modifier(Modifier::DIM);
    let mut lines = vec![Line::default()];
    lines.push(Line::from(vec![
        Span::styled(prefix, tag_style),
        Span::styled(first.to_owned(), body_style),
        Span::styled(hint, hint_style),
    ]));
    lines.extend(text.lines().skip(1).map(|line| {
        Line::from(vec![
            Span::styled("│ ".to_owned(), body_style),
            Span::styled(line.to_owned(), body_style),
        ])
    }));
    wrap_commit_lines(lines, width)
}

fn static_tool_lines(tool: &ToolBlock, width: u16) -> Vec<(String, Color)> {
    let mut lines = tool.commit_lines();
    if tool.has_details() && lines.len() == 1 {
        if let Some((summary, _)) = lines.first_mut() {
            summary.push_str(&format!(
                "  [folded · Ctrl+O details · {} rows]",
                tool.details_count()
            ));
        }
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(index, (text, color))| {
            if index == 0 {
                let prefix = if width >= 72 {
                    format!("◈ {} ", tool.phase_label())
                } else {
                    format!("{} ", tool.phase_short_label())
                };
                (format!("{prefix}{text}"), color)
            } else {
                (format!("  ┆ {text}"), color)
            }
        })
        .collect()
}

fn commit_lines(
    text: String,
    color: Color,
    markdown: bool,
    partial: bool,
    modifier: Modifier,
    width: u16,
) -> Vec<Line<'static>> {
    commit_lines_with_answer_metrics(text, color, markdown, partial, modifier, width, None)
}

fn commit_lines_with_answer_metrics(
    text: String,
    color: Color,
    markdown: bool,
    partial: bool,
    modifier: Modifier,
    width: u16,
    metrics: Option<PresentationMetrics>,
) -> Vec<Line<'static>> {
    let text = sanitize_display_text(&text);
    let text = if markdown {
        bound_answer_history_text(&text)
    } else {
        text
    };
    let text = fold_lines(&text, FOLD_MAX);
    let mut lines: Vec<Line> = vec![Line::default()];
    if markdown {
        lines.extend(answer_commit_lines_with_status_and_metrics(
            &text, partial, metrics,
        ));
    } else {
        lines.extend(text.lines().map(|l| {
            Line::from(Span::styled(
                l.to_owned(),
                Style::default().fg(color).add_modifier(modifier),
            ))
        }));
    }
    wrap_commit_lines(lines, width)
}

fn colored_commit_lines(entries: Vec<(String, Color)>, width: u16) -> Vec<Line<'static>> {
    let rows = entries
        .into_iter()
        .flat_map(|(text, color)| {
            let text = fold_lines(&sanitize_display_text(&text), FOLD_MAX);
            text.lines()
                .map(move |line| (line.to_owned(), color))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut lines = vec![Line::default()];
    lines.extend(
        rows.into_iter()
            .map(|(text, color)| Line::from(Span::styled(text, Style::default().fg(color)))),
    );
    wrap_commit_lines(lines, width)
}

const MAX_COMMIT_INSERT_ROWS: usize = 8;

/// Pre-wrap committed rows and insert them in small batches.  A huge logical
/// line otherwise becomes one enormous `insert_before` scroll operation; raw
/// ConPTY can discard that range even though the in-memory archive is intact.
#[derive(Clone, Copy)]
struct CommitSemanticPrefix {
    first: &'static str,
    continuation: &'static str,
}

fn commit_semantic_prefix(spans: &[Span<'static>]) -> Option<CommitSemanticPrefix> {
    let first = spans.first()?.content.as_ref();
    [
        ("\u{256d} ANSWER ", "\u{2502} "),
        ("\u{2502} ", "\u{2502} "),
        ("\u{257a} ", "\u{2502} "),
        ("\u{250a} ", "\u{2502} "),
        ("C ", "  \u{2506} "),
        ("O ", "  \u{2506} "),
        ("T ", "  \u{2506} "),
        ("\u{25c8} CALL ", "  \u{2506} "),
        ("\u{25c8} OUT ", "  \u{2506} "),
        ("\u{25c8} TOOL ", "  \u{2506} "),
        ("\u{25c8} ", "  \u{2506} "),
        ("  \u{2506} ", "  \u{2506} "),
    ]
    .into_iter()
    .find_map(|(prefix, continuation)| {
        first.starts_with(prefix).then_some(CommitSemanticPrefix {
            first: prefix,
            continuation,
        })
    })
}

fn split_commit_prefix(
    spans: Vec<Span<'static>>,
    prefix: &str,
) -> Option<(Vec<Span<'static>>, Vec<Span<'static>>)> {
    let joined = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    if !joined.starts_with(prefix) {
        return None;
    }

    let mut prefix_spans = Vec::new();
    let mut body_spans = Vec::new();
    let mut remaining = prefix.len();
    for span in spans {
        let content = span.content.into_owned();
        if remaining == 0 {
            body_spans.push(Span::styled(content, span.style));
            continue;
        }
        let take = remaining.min(content.len());
        if !content.is_char_boundary(take) {
            return None;
        }
        let (head, tail) = content.split_at(take);
        if !head.is_empty() {
            prefix_spans.push(Span::styled(head.to_owned(), span.style));
        }
        if !tail.is_empty() {
            body_spans.push(Span::styled(tail.to_owned(), span.style));
        }
        remaining -= take;
    }
    (remaining == 0).then_some((prefix_spans, body_spans))
}

#[derive(Clone)]
struct CommitFragment {
    text: String,
    style: Style,
    cells: usize,
}

enum CommitBodyUnit {
    Word(Vec<CommitFragment>),
    Whitespace(Vec<CommitFragment>),
    Newline,
}

fn commit_body_units(body: Vec<Span<'static>>) -> Vec<CommitBodyUnit> {
    let mut units = Vec::new();
    let mut kind = None;
    let mut fragments = Vec::new();

    for span in body {
        let style = span.style;
        for grapheme in span.content.as_ref().graphemes(true) {
            if grapheme == "\n" {
                if let Some(kind) = kind.take() {
                    let chunk = std::mem::take(&mut fragments);
                    units.push(if kind {
                        CommitBodyUnit::Whitespace(chunk)
                    } else {
                        CommitBodyUnit::Word(chunk)
                    });
                }
                units.push(CommitBodyUnit::Newline);
                continue;
            }

            let whitespace = grapheme.chars().all(char::is_whitespace);
            if kind.is_some_and(|current| current != whitespace) {
                let previous = kind.take().expect("commit unit kind exists");
                let chunk = std::mem::take(&mut fragments);
                units.push(if previous {
                    CommitBodyUnit::Whitespace(chunk)
                } else {
                    CommitBodyUnit::Word(chunk)
                });
            }
            kind = Some(whitespace);
            fragments.push(CommitFragment {
                text: grapheme.to_owned(),
                style,
                cells: str_cells(grapheme),
            });
        }
    }

    if let Some(kind) = kind {
        units.push(if kind {
            CommitBodyUnit::Whitespace(fragments)
        } else {
            CommitBodyUnit::Word(fragments)
        });
    }
    units
}

fn commit_fragments_cells(fragments: &[CommitFragment]) -> usize {
    fragments.iter().map(|fragment| fragment.cells).sum()
}

fn push_commit_continuation(
    rows: &mut Vec<Vec<Span<'static>>>,
    continuation: &str,
    prefix_style: Style,
) {
    rows.push(vec![Span::styled(continuation.to_owned(), prefix_style)]);
}

fn append_commit_fragments(
    rows: &mut Vec<Vec<Span<'static>>>,
    fragments: &[CommitFragment],
    row_cells: &mut usize,
    width: usize,
    continuation: &str,
    continuation_cells: usize,
    prefix_style: Style,
) {
    for fragment in fragments {
        if *row_cells > 0 && row_cells.saturating_add(fragment.cells) > width {
            push_commit_continuation(rows, continuation, prefix_style);
            *row_cells = continuation_cells;
        }
        rows.last_mut()
            .expect("semantic commit wrap owns one row")
            .push(Span::styled(fragment.text.clone(), fragment.style));
        *row_cells = row_cells.saturating_add(fragment.cells);
    }
}

fn activity_commit_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("⟦")?;
    let close = rest.find("⟧ ")?;
    let header = &rest[..close];
    let (tag, sequence) = header.split_once(" #")?;
    if !matches!(
        tag,
        "SYS"
            | "PLAN"
            | "THK"
            | "ANS"
            | "TLS"
            | "CHK"
            | "SUM"
            | "WAIT"
            | "ASK"
            | "QUE"
            | "TAKE"
            | "DONE"
            | "ERR"
    ) || sequence.is_empty()
        || !sequence.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let end = "⟦".len() + close + "⟧ ".len();
    text.get(..end)
}

fn wrap_semantic_commit_line_with_prefix(
    spans: Vec<Span<'static>>,
    first: &str,
    continuation: &str,
    width: u16,
) -> Option<Vec<Line<'static>>> {
    let (first_prefix, body) = split_commit_prefix(spans, first)?;
    let first_cells = str_cells(first);
    let continuation_cells = str_cells(continuation);
    let width = width as usize;
    if width <= first_cells || width <= continuation_cells {
        return None;
    }

    let prefix_style = first_prefix
        .first()
        .map(|span| span.style)
        .unwrap_or_default();
    let mut rows = vec![first_prefix];
    let mut row_cells = first_cells;
    let mut row_has_body = false;
    let mut pending_whitespace = Vec::new();
    for unit in commit_body_units(body) {
        match unit {
            CommitBodyUnit::Newline => {
                pending_whitespace.clear();
                push_commit_continuation(&mut rows, continuation, prefix_style);
                row_cells = continuation_cells;
                row_has_body = false;
            }
            CommitBodyUnit::Whitespace(fragments) => {
                pending_whitespace = fragments;
            }
            CommitBodyUnit::Word(fragments) => {
                let whitespace_cells = commit_fragments_cells(&pending_whitespace);
                let word_cells = commit_fragments_cells(&fragments);
                let would_overflow = row_cells
                    .saturating_add(whitespace_cells)
                    .saturating_add(word_cells)
                    > width;
                if row_has_body && would_overflow {
                    push_commit_continuation(&mut rows, continuation, prefix_style);
                    row_cells = continuation_cells;
                    pending_whitespace.clear();
                } else if !row_has_body && would_overflow {
                    pending_whitespace.clear();
                }
                append_commit_fragments(
                    &mut rows,
                    &pending_whitespace,
                    &mut row_cells,
                    width,
                    continuation,
                    continuation_cells,
                    prefix_style,
                );
                append_commit_fragments(
                    &mut rows,
                    &fragments,
                    &mut row_cells,
                    width,
                    continuation,
                    continuation_cells,
                    prefix_style,
                );
                pending_whitespace.clear();
                row_has_body = true;
            }
        }
    }
    Some(rows.into_iter().map(Line::from).collect())
}

fn wrap_semantic_commit_line(spans: Vec<Span<'static>>, width: u16) -> Option<Vec<Line<'static>>> {
    let prefix = commit_semantic_prefix(&spans)?;
    wrap_semantic_commit_line_with_prefix(spans, prefix.first, prefix.continuation, width)
}

fn wrap_commit_line(line: Line<'static>, width: u16) -> Vec<Line<'static>> {
    let spans = line.spans.into_iter().collect::<Vec<_>>();
    let activity_prefix = spans
        .first()
        .and_then(|span| activity_commit_prefix(span.content.as_ref()))
        .map(str::to_owned);
    if let Some(prefix) = activity_prefix.as_deref() {
        if let Some(wrapped) =
            wrap_semantic_commit_line_with_prefix(spans.clone(), prefix, "│ ", width)
        {
            return wrapped;
        }
    }
    wrap_semantic_commit_line(spans.clone(), width).unwrap_or_else(|| {
        wrap_live_spans_greedy(spans, width)
            .into_iter()
            .map(Line::from)
            .collect()
    })
}

pub(crate) fn wrap_commit_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .flat_map(|line| wrap_commit_line(line, width))
        .collect()
}

fn insert_bounded_commit_lines<B: Backend>(
    terminal: &mut Terminal<B>,
    mut lines: Vec<Line<'static>>,
) -> io::Result<()> {
    while !lines.is_empty() {
        let count = lines.len().min(MAX_COMMIT_INSERT_ROWS);
        let batch = lines.drain(..count).collect::<Vec<_>>();
        terminal.insert_before(count as u16, |buf| {
            Paragraph::new(Text::from(batch))
                .wrap(Wrap { trim: false })
                .render(buf.area, buf);
        })?;
    }
    Ok(())
}
