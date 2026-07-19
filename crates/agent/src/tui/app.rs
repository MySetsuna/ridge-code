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

#[derive(Default)]
pub(crate) struct Ui {
    pub(crate) input: InputState,
    /// 补全浮窗(iter-27):Some = 浮窗开(键位模态优先级:审批 > 浮窗 > 输入)。
    pub(crate) popup: Option<Popup>,
    /// 待静态提交队列(iter-26):`note` 只入队,主环 drain 经 `insert_before` 写进终端原生历史。
    /// 队列是瞬态的(每圈清空),无需环形上限 —— 有界性由「提交即出队」保证。
    pub(crate) commits: Vec<(String, Color)>,
    pub(crate) stream: String,
    pub(crate) todos: Vec<Todo>,
    pub(crate) scroll: u16,
    pub(crate) busy: bool,
    pub(crate) phase: String,
    pub(crate) frame: usize,
    /// 启动帧序列进度(iter-28):< SPLASH_TICKS 时 tick 驱动渐显,末帧 banner 入历史。
    pub(crate) splash: usize,
    /// 本任务流式 token 估算累计(iter-31):token_rx 每块 `est_tokens` 累加,Submit 清零、done 保留展示。
    pub(crate) stream_tokens: usize,
    /// 排队待跑的提交(iter-33):busy 时 Enter 入队,任务 done 后自动取队首接跑;中断清空。
    pub(crate) queued: VecDeque<String>,
    /// 交互页(iter-35):Some = 模态页开(键位模态优先级:审批 > Panel > 浮窗 > 输入)。
    pub(crate) panel: Option<Panel>,
    /// 自定义/skill 命令请求「以任务身份跑」(iter-39):`run_command` 展开 body 置此,主环取走起任务。
    pub(crate) run_task: Option<String>,
}
impl Ui {
    pub(crate) fn note(&mut self, text: impl Into<String>, color: Color) {
        self.commits.push((text.into(), color));
    }
    pub(crate) fn drain_commits(&mut self) -> Vec<(String, Color)> {
        std::mem::take(&mut self.commits)
    }
}

/// 把积压的历史行静态提交进终端 scrollback(iter-26 核心):行一经 `insert_before`
/// 即成原生历史,永不参与后续帧的差分重绘 —— Live 视口恒小,闪烁根因根除。
pub(crate) fn flush_commits(terminal: &mut Term, ui: &mut Ui) -> io::Result<()> {
    let width = terminal.size()?.width;
    for (text, color) in ui.drain_commits() {
        let text = fold_lines(&text, FOLD_MAX); // 呈现层折叠(iter-28):历史不刷屏
                                                // 块间前置一空白行(iter-31 需求 5):连续输出块视觉分栏,不再贴成一片。
        let h = commit_height(&text, width) + 1;
        terminal.insert_before(h, |buf| {
            let mut lines: Vec<Line> = vec![Line::default()];
            if text.starts_with("🤖") {
                // 终答走 md 轻渲染(iter-28):样式已定型,提交时染。
                let mut in_code = false;
                lines.extend(text.lines().map(|l| {
                    let (spans, next) = md_line_spans(l, in_code);
                    in_code = next;
                    Line::from(spans)
                }));
            } else {
                lines.extend(
                    text.lines().map(|l| {
                        Line::from(Span::styled(l.to_owned(), Style::default().fg(color)))
                    }),
                );
            }
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .render(buf.area, buf);
        })?;
    }
    Ok(())
}
