use agent::Config;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::transcript::LiveBlockFocus;
use super::{fmt_ctx, ActivityEntry, AnswerEntry, LiveTranscript, ReasoningEntry, ToolBlock};
use crate::config_path;
use agent::PROVIDER_PRESETS;

const DETAIL_SCROLL_STEP: i32 = 4;

/// 交互页类别:决定 Enter 动作与提示文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKind {
    /// 配置页:Enter 就地编辑选中键的值。
    Config,
    /// 工具页:只读浏览 + 搜索。
    Tools,
    /// 已提交工具历史:摘要默认收起,Enter 在预览窗展开详情。
    ToolHistory,
    /// 已提交 reasoning 历史:保留原生 scrollback，同时提供检索与展开详情。
    ReasoningHistory,
    /// Recoverable Answer archive remains searchable after scrollback folding.
    AnswerHistory,
    /// 当前流式 Answer/Reasoning/Tool 混合块:只读聚焦与按需展开。
    LiveHistory,
    /// Agent 最近阶段/事件:只读 bounded 时间线。
    Activity,
    /// 忙碌任务的 FIFO 队列：可观察并删除尚未执行的单条意图。
    Queue,
    /// 模型页:Enter 热切换到选中模型 + 缓存 ctx_window。
    Models,
    /// Model selection second stage: choose reasoning effort after choosing a model.
    Effort,
    /// Sub-agent 页:只读浏览 + 搜索。
    Agent,
    /// 登录页(iter-38):↑↓ 选内置供应商 → Enter 就地输入 key(掩码)→ Enter 校验并接入。
    Login,
    /// MCP 页(iter-40):↑↓ 选 MCP → Enter 查看详情/管理。
    Mcp,
    /// Skills 页(iter-40):↑↓ 选技能 → Enter 查看详情/管理。
    Skills,
}

/// 一行:动作键(config 键 / provider 名 / 模型 id / 工具名 / agent 名)+ 右列值 + (模型)上下文窗口。
pub(crate) struct PanelRow {
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) ctx: Option<u64>,
}

/// Presentation-only action attached to a filtered row. Keeping this out of
/// the row text avoids brittle prefix parsing for pending-message controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelRowAction {
    None,
    RemoveQueued(usize),
    FocusLiveBlock(LiveBlockFocus),
}

/// 模态交互页(iter-35):标题 + 搜索框(随打随滤)+ 过滤列表(选中高亮)+ 选中动作。
/// `view` 是 `rows` 的过滤下标,`sel` 是 `view` 内位次;`editing=Some` = 配置页正编辑选中键值。
pub(crate) struct Panel {
    pub(crate) kind: PanelKind,
    pub(crate) title: String,
    pub(crate) query: String,
    pub(crate) rows: Vec<PanelRow>,
    pub(crate) row_actions: Vec<PanelRowAction>,
    pub(crate) view: Vec<usize>,
    pub(crate) sel: usize,
    pub(crate) editing: Option<String>,
    pub(crate) oauth_verifier: Option<String>,
    pub(crate) oauth_state: Option<String>,
    pub(crate) oauth_redirect_uri: Option<String>,
    pub(crate) detail_open: bool,
    /// Manual visual-row adjustment around an automatic search hit.
    pub(crate) detail_scroll: i32,
    /// Monotonic presentation identity for the current row/detail snapshot.
    /// Rebuilt live panels get a new identity; selection/query changes do not.
    pub(crate) content_revision: u64,
}

/// 过滤:key/value 不分大小写子串命中;空 query = 全含。有序稳态(保 rows 原序)。纯函数。
pub(crate) fn panel_filter(rows: &[PanelRow], query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, r)| {
            q.is_empty() || r.key.to_lowercase().contains(&q) || r.value.to_lowercase().contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}

impl Panel {
    pub(crate) fn new(kind: PanelKind, title: String, rows: Vec<PanelRow>) -> Self {
        let view = panel_filter(&rows, "");
        static NEXT_CONTENT_REVISION: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        Panel {
            kind,
            title,
            query: String::new(),
            rows,
            row_actions: vec![PanelRowAction::None; view.len()],
            view,
            sel: 0,
            editing: None,
            oauth_verifier: None,
            oauth_state: None,
            oauth_redirect_uri: None,
            detail_open: false,
            detail_scroll: 0,
            content_revision: NEXT_CONTENT_REVISION
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
    /// query 变更后重算 view 并把 sel 钳回范围内。
    pub(crate) fn retype(&mut self) {
        self.detail_scroll = 0;
        self.view = panel_filter(&self.rows, &self.query);
        if self.sel >= self.view.len() {
            self.sel = self.view.len().saturating_sub(1);
        }
        if self.supports_detail() && !self.query.is_empty() {
            let query = self.query.to_lowercase();
            if let Some(detail_sel) = self
                .view
                .iter()
                .position(|&index| self.rows[index].value.to_lowercase().contains(&query))
            {
                self.sel = detail_sel;
                self.detail_open = true;
            } else {
                self.detail_open = false;
            }
        }
    }
    pub(crate) fn move_up(&mut self) {
        self.detail_scroll = 0;
        self.sel = self.sel.saturating_sub(1);
    }
    pub(crate) fn move_down(&mut self) {
        self.detail_scroll = 0;
        if self.sel + 1 < self.view.len() {
            self.sel += 1;
        }
    }
    pub(crate) fn page_up(&mut self) {
        self.detail_scroll = 0;
        self.sel = self.sel.saturating_sub(8);
    }
    pub(crate) fn page_down(&mut self) {
        self.detail_scroll = 0;
        if !self.view.is_empty() {
            self.sel = (self.sel + 8).min(self.view.len() - 1);
        }
    }
    pub(crate) fn first(&mut self) {
        self.detail_scroll = 0;
        self.sel = 0;
    }
    pub(crate) fn last(&mut self) {
        self.detail_scroll = 0;
        self.sel = self.view.len().saturating_sub(1);
    }
    pub(crate) fn toggle_detail(&mut self) -> bool {
        if !self.supports_detail() {
            return false;
        }
        self.detail_scroll = 0;
        self.detail_open = !self.detail_open;
        self.detail_open
    }
    /// Move the expanded detail view around its search anchor.
    pub(crate) fn scroll_detail(&mut self, delta: i8) -> bool {
        if !self.supports_detail() || !self.detail_open {
            return false;
        }
        let before = self.detail_scroll;
        if delta > 0 {
            self.detail_scroll = self.detail_scroll.saturating_add(DETAIL_SCROLL_STEP);
        } else if delta < 0 {
            self.detail_scroll = self.detail_scroll.saturating_sub(DETAIL_SCROLL_STEP);
        }
        self.detail_scroll != before
    }

    pub(crate) fn supports_detail(&self) -> bool {
        matches!(
            self.kind,
            PanelKind::Activity
                | PanelKind::ToolHistory
                | PanelKind::ReasoningHistory
                | PanelKind::AnswerHistory
                | PanelKind::LiveHistory
        )
    }
    pub(crate) fn selected(&self) -> Option<&PanelRow> {
        self.view.get(self.sel).map(|&i| &self.rows[i])
    }

    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.view.get(self.sel).copied()
    }

    pub(crate) fn selected_action(&self) -> PanelRowAction {
        self.selected_index()
            .and_then(|index| self.row_actions.get(index).copied())
            .unwrap_or(PanelRowAction::None)
    }

    pub(crate) fn with_row_actions(mut self, actions: Vec<PanelRowAction>) -> Self {
        debug_assert_eq!(self.rows.len(), actions.len());
        if self.rows.len() == actions.len() {
            self.row_actions = actions;
        }
        self
    }

    /// Only observer panels may replace one another without discarding an edit.
    pub(crate) fn allows_attention_switch(&self) -> bool {
        self.editing.is_none()
            && matches!(
                self.kind,
                PanelKind::ToolHistory
                    | PanelKind::ReasoningHistory
                    | PanelKind::AnswerHistory
                    | PanelKind::LiveHistory
                    | PanelKind::Activity
                    | PanelKind::Queue
            )
    }
}

/// 当前 config 某标量键的显示值(空 → 空串,由调用方替 `(未设)`)。
pub(crate) fn config_value(cfg: &Config, key: &str) -> String {
    match key {
        "provider" => cfg.provider.clone().unwrap_or_default(),
        "model" => cfg.model.clone().unwrap_or_default(),
        "effort" => cfg.effort.clone().unwrap_or_default(),
        "base_url" => cfg.base_url.clone().unwrap_or_default(),
        "budget_tokens" => cfg.budget_tokens.map(|n| n.to_string()).unwrap_or_default(),
        "skills_dir" => cfg.skills_dir.clone().unwrap_or_default(),
        "skip_danger" => cfg.skip_danger.map(|b| b.to_string()).unwrap_or_default(),
        "status_bar" => cfg.status_bar.clone().unwrap_or_default(),
        "allow_jailbreak" => cfg
            .allow_jailbreak
            .map(|b| b.to_string())
            .unwrap_or_default(),
        "proxy" => cfg.proxy.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// 配置页:行取自 `CONFIG_KEYS`,值现读现显(缺 → `(未设)`)。
pub(crate) fn config_panel() -> Panel {
    let cfg = Config::load(config_path());
    let rows = agent::CONFIG_KEYS
        .iter()
        .map(|k| {
            let v = config_value(&cfg, k);
            PanelRow {
                key: (*k).to_string(),
                value: if v.is_empty() { "(unset)".into() } else { v },
                ctx: None,
            }
        })
        .collect();
    Panel::new(
        PanelKind::Config,
        "Config · ↑↓ select · Enter edit · type to filter · Esc close".into(),
        rows,
    )
}

/// 工具页(只读):列工具名。
pub(crate) fn tools_panel(tools: &[String]) -> Panel {
    let rows = tools
        .iter()
        .map(|t| PanelRow {
            key: t.clone(),
            value: String::new(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Tools,
        "Tools (read-only) · type to filter · Esc close".into(),
        rows,
    )
}

/// 已提交工具历史:保留原生 scrollback 不变,在模态预览窗按需展开完整详情。
pub(crate) fn tool_history_panel(history: &std::collections::VecDeque<ToolBlock>) -> Panel {
    let rows = history
        .iter()
        .rev()
        .enumerate()
        .map(|(index, tool)| PanelRow {
            key: format!(
                "#{} {} · p#{}",
                index + 1,
                tool.summary(),
                tool.presentation_id()
            ),
            value: tool.details_text(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::ToolHistory,
        "Tool history · ↑↓/PgUp/PgDn select · Enter expand · type to filter · Esc close".into(),
        rows,
    )
}

/// 已提交 reasoning 历史:摘要展示 THINK/step/token/字符数，Enter 展开完整思考文本。
pub(crate) fn reasoning_history_panel(
    history: &std::collections::VecDeque<ReasoningEntry>,
) -> Panel {
    let rows = history
        .iter()
        .rev()
        .enumerate()
        .map(|(index, reasoning)| PanelRow {
            key: format!(
                "#{} THINK · step {} · {} tok · +{}s · {} chars · p#{}",
                index + 1,
                reasoning.step,
                reasoning.tokens,
                reasoning.elapsed_s,
                reasoning.text.chars().count(),
                reasoning.id
            ),
            value: reasoning.text.clone(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::ReasoningHistory,
        "Reasoning history · ↑↓/PgUp/PgDn select · Enter expand · type to filter · Esc close"
            .into(),
        rows,
    )
}

/// Recoverable Answer archive: newest first, full body available on Enter;
/// interrupted streams are visibly marked PARTIAL.
pub(crate) fn answer_history_panel(history: &std::collections::VecDeque<AnswerEntry>) -> Panel {
    let rows = history
        .iter()
        .rev()
        .enumerate()
        .map(|(index, answer)| PanelRow {
            key: format!(
                "#{} {} · step {} · {} tok · +{}s · {} chars · p#{}",
                index + 1,
                if answer.partial { "PARTIAL" } else { "ANSWER" },
                answer.step,
                answer.tokens,
                answer.elapsed_s,
                answer.text.chars().count(),
                answer.id
            ),
            value: answer.text.clone(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::AnswerHistory,
        "Answer archive · completed + partial · ↑↓/PgUp/PgDn select · Enter expand · type to filter · Esc close".into(),
        rows,
    )
}

/// 当前流式混合块:模型输出不离开 LiveTranscript,只在 Inspector 中聚焦/展开。
/// Live inspector plus an actionable pending FIFO rail. Queue rows remain
/// presentation-only; the main loop performs the actual removal.
pub(crate) fn live_history_panel_with_queue(
    transcript: &LiveTranscript,
    queue: &std::collections::VecDeque<String>,
) -> Panel {
    let entries = transcript.inspector_rows();
    let mut rows = Vec::with_capacity(entries.len() + queue.len());
    let mut actions = Vec::with_capacity(entries.len() + queue.len());
    for entry in entries {
        rows.push(PanelRow {
            key: entry.key,
            value: entry.detail,
            ctx: None,
        });
        actions.push(PanelRowAction::FocusLiveBlock(entry.focus));
    }
    for (index, _message) in queue.iter().enumerate() {
        rows.push(PanelRow {
            key: if index == 0 {
                "⏭ pending · next".into()
            } else {
                format!("⏳ pending · #{}", index + 1)
            },
            value: "queued message".into(),
            ctx: None,
        });
        actions.push(PanelRowAction::RemoveQueued(index));
    }
    Panel::new(
        PanelKind::LiveHistory,
        "TRANSCRIPT AUDIT · Live blocks + pending · ↑↓/PgUp/PgDn select · Enter/Space expand · Delete pending · Ctrl+Q queue · Esc close".into(),
        rows,
    )
    .with_row_actions(actions)
}

/// Agent 活动时间线:只保留最近五个真实状态转移，最新项置顶，详情仍可通过正文/工具面板查看。
pub(crate) fn activity_panel(history: &std::collections::VecDeque<ActivityEntry>) -> Panel {
    let rows = if history.is_empty() {
        vec![PanelRow {
            key: "—".into(),
            value: "no activity observed yet".into(),
            ctx: None,
        }]
    } else {
        history
            .iter()
            .rev()
            .enumerate()
            .map(|(index, entry)| PanelRow {
                key: if index == 0 {
                    format!("{} now", entry.kind.tag())
                } else {
                    format!("{} #{}", entry.kind.tag(), entry.sequence)
                },
                value: entry.text.clone(),
                ctx: None,
            })
            .collect()
    };
    Panel::new(
        PanelKind::Activity,
        format!(
            "Activity · audit {} · ↑↓ select · Enter expand · Alt+PgUp/PgDn detail · Esc close",
            history.len()
        ),
        rows,
    )
}

/// 忙碌任务队列：显示真实 FIFO 顺序，编辑/置前发送只影响尚未启动的意图。
pub(crate) fn queue_panel(queue: &std::collections::VecDeque<String>) -> Panel {
    let rows = if queue.is_empty() {
        vec![PanelRow {
            key: "—".into(),
            value: "queue empty".into(),
            ctx: None,
        }]
    } else {
        queue
            .iter()
            .enumerate()
            .map(|(index, _message)| PanelRow {
                key: if index == 0 {
                    "⏭ next".into()
                } else {
                    format!("⏳ #{}", index + 1)
                },
                value: "queued message".into(),
                ctx: None,
            })
            .collect()
    };
    Panel::new(
        PanelKind::Queue,
        "Queue · ↑↓ select · Enter edit · Ctrl+Enter send now · Delete remove · Esc close".into(),
        rows,
    )
}

/// 模型页:跨 provider 列实时模型(`provider · id` → ctx),`sel` 落当前 provider+模型。
pub(crate) fn models_panel(
    grouped: &[(String, Vec<provider::models::ModelInfo>)],
    current_provider: &str,
    current_model: &str,
) -> Panel {
    let rows: Vec<PanelRow> = grouped
        .iter()
        .flat_map(|(provider, models)| {
            models.iter().map(move |m| PanelRow {
                key: format!("{} · {}", provider, m.id),
                value: format!(
                    "ctx {}",
                    m.context.map(fmt_ctx).unwrap_or_else(|| "?".into())
                ),
                ctx: m.context,
            })
        })
        .collect();
    let target = format!("{} · {}", current_provider, current_model);
    let mut p = Panel::new(
        PanelKind::Models,
        "Models · provider/model · ↑↓ select · Enter next · type to filter · Esc close".into(),
        rows,
    );
    if let Some(pos) = p.view.iter().position(|&i| p.rows[i].key == target) {
        p.sel = pos;
    }
    p
}

/// Reasoning effort is a second-stage choice, shown only after a model row is
/// selected (or directly through `/effort`).
pub(crate) fn effort_panel(current_effort: &str) -> Panel {
    let rows = provider::REASONING_EFFORTS
        .iter()
        .map(|effort| PanelRow {
            key: (*effort).to_string(),
            value: if *effort == current_effort {
                "current".into()
            } else {
                "set reasoning effort".into()
            },
            ctx: None,
        })
        .collect();
    let mut panel = Panel::new(
        PanelKind::Effort,
        "Reasoning effort · ↑↓ select · Enter apply · Esc close".into(),
        rows,
    );
    if let Some(position) = panel
        .view
        .iter()
        .position(|&index| panel.rows[index].key == current_effort)
    {
        panel.sel = position;
    }
    panel
}

/// Sub-agent 页(只读):列 agent 名 + 描述。
pub(crate) fn agent_panel(defs: &[agent::Agent]) -> Panel {
    let rows = defs
        .iter()
        .map(|a| PanelRow {
            key: a.name.clone(),
            value: a.description.clone(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Agent,
        "Sub-agents (read-only) · type to filter · Esc close".into(),
        rows,
    )
}

pub(crate) const CLAUDE_OAUTH_ROW: &str = "claude-oauth";
pub(crate) const CODEX_OAUTH_ROW: &str = "codex-oauth";

/// 登录页(iter-38):列内置供应商 preset(id · label · model),Enter 选中后就地输入 key;
/// 另含订阅 OAuth 入口(iter-43 Claude / iter-48 ChatGPT Codex),授权码登录。
pub(crate) fn login_panel() -> Panel {
    let mut rows = vec![
        PanelRow {
            key: CLAUDE_OAUTH_ROW.to_string(),
            value: "Claude OAuth subscription · browser auth code".to_string(),
            ctx: None,
        },
        PanelRow {
            key: CODEX_OAUTH_ROW.to_string(),
            value: "ChatGPT Plus/Pro OAuth (Codex) · browser auth".to_string(),
            ctx: None,
        },
    ];
    rows.extend(PROVIDER_PRESETS.iter().map(|p| PanelRow {
        key: p.id.to_string(),
        value: format!("{} · {}", p.label, p.default_model),
        ctx: None,
    }));
    Panel::new(
        PanelKind::Login,
        "Login · Enter key login or subscription OAuth · type to filter · Esc close".into(),
        rows,
    )
}

/// MCP 页(只读):列 config 里已配置的 MCP 服务器(名 · 命令[+参数])。直读真实 `Config.mcp`。
/// 本架构 MCP 由 `resolve_mcp` 每会话临起,无常驻进程可 start/stop —— 故只读展示,不做进程管理。
pub(crate) fn mcp_command_label(cmd: &str, args: &[String]) -> String {
    let mut parts = vec![cmd.to_string()];
    let mut redact_next = false;
    for arg in args {
        if redact_next {
            parts.push("<redacted>".into());
            redact_next = false;
            continue;
        }
        let lower = arg.to_ascii_lowercase();
        if let Some((name, _)) = arg.split_once('=') {
            if mcp_secret_name(name) {
                parts.push(format!("{name}=<redacted>"));
                continue;
            }
        }
        if mcp_secret_name(&lower) {
            parts.push(arg.clone());
            redact_next = true;
        } else {
            parts.push(arg.clone());
        }
    }
    parts.join(" ")
}

fn mcp_secret_name(value: &str) -> bool {
    let name = value
        .trim_start_matches('-')
        .replace('_', "-")
        .to_ascii_lowercase();
    name == "key"
        || name == "token"
        || name == "secret"
        || name == "password"
        || name == "credential"
        || name == "authorization"
        || name == "cookie"
        || name == "header"
        || name.ends_with("-key")
        || name.ends_with("-token")
        || name.ends_with("-secret")
        || name.ends_with("-password")
        || name.ends_with("-credential")
}

pub(crate) fn mcp_panel(statuses: &[agent::McpServerStatus]) -> Panel {
    let cfg = Config::load(config_path());
    let mut names = std::collections::BTreeSet::new();
    let mut rows: Vec<PanelRow> = cfg
        .mcp
        .iter()
        .map(|m| {
            names.insert(m.name.clone());
            let command = mcp_command_label(&m.cmd, &m.args);
            let value = match statuses.iter().find(|status| status.name == m.name) {
                Some(status) => format!(
                    "{command} · {} · {}",
                    status.trail_labels().join(" → "),
                    status.detail
                ),
                None => format!("{command} · configured · not started"),
            };
            PanelRow {
                key: m.name.clone(),
                value,
                ctx: None,
            }
        })
        .collect();
    for status in statuses
        .iter()
        .filter(|status| !names.contains(&status.name))
    {
        rows.push(PanelRow {
            key: status.name.clone(),
            value: format!(
                "{} · {} · {}",
                status.trail_labels().join(" → "),
                status.state.label(),
                status.detail
            ),
            ctx: None,
        });
    }
    Panel::new(
        PanelKind::Mcp,
        "MCP servers · runtime status (read-only) · type to filter · Esc close".into(),
        rows,
    )
}

/// Skills 页(只读):列本会话已加载的技能(名 · 描述)。
pub(crate) fn skills_panel(skills: &[agent::Skill]) -> Panel {
    let rows = skills
        .iter()
        .map(|s| PanelRow {
            key: s.name.clone(),
            value: s.description.clone(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Skills,
        "Skills (read-only) · type to filter · Esc close".into(),
        rows,
    )
}

/// 交互页键路由(iter-35,纯函数):模态优先级 审批 > Panel > 浮窗 > 输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelAction {
    Up,
    Down,
    PageUp,
    PageDown,
    DetailPageUp,
    DetailPageDown,
    First,
    Last,
    Enter,
    SendNow,
    Esc,
    Remove,
    Char(char),
    Backspace,
    Ignore,
}

pub(crate) fn panel_action(key: &KeyEvent) -> PanelAction {
    if key.kind != KeyEventKind::Press {
        return PanelAction::Ignore;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::PageUp => return PanelAction::DetailPageUp,
            KeyCode::PageDown => return PanelAction::DetailPageDown,
            _ => {}
        }
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Enter {
        return PanelAction::SendNow;
    }
    match key.code {
        KeyCode::Up => PanelAction::Up,
        KeyCode::Down => PanelAction::Down,
        KeyCode::PageUp => PanelAction::PageUp,
        KeyCode::PageDown => PanelAction::PageDown,
        KeyCode::Home => PanelAction::First,
        KeyCode::End => PanelAction::Last,
        KeyCode::Enter => PanelAction::Enter,
        KeyCode::Esc => PanelAction::Esc,
        KeyCode::Delete => PanelAction::Remove,
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => PanelAction::Remove,
        KeyCode::Backspace => PanelAction::Backspace,
        KeyCode::Char(c) => PanelAction::Char(c),
        _ => PanelAction::Ignore,
    }
}
