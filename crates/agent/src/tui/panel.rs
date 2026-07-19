use super::*;

/// 交互页类别:决定 Enter 动作与提示文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKind {
    /// 配置页:Enter 就地编辑选中键的值。
    Config,
    /// Provider 页:Enter 热切换到选中档。
    Provider,
    /// 工具页:只读浏览 + 搜索。
    Tools,
    /// 模型页:Enter 热切换到选中模型 + 缓存 ctx_window。
    Models,
    /// Sub-agent 页:只读浏览 + 搜索。
    Agent,
    /// 登录页(iter-38):↑↓ 选内置供应商 → Enter 就地输入 key(掩码)→ Enter 校验并接入。
    Login,
}

/// 一行:动作键(config 键 / provider 名 / 模型 id / 工具名 / agent 名)+ 右列值 + (模型)上下文窗口。
pub(crate) struct PanelRow {
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) ctx: Option<u64>,
}

/// 模态交互页(iter-35):标题 + 搜索框(随打随滤)+ 过滤列表(选中高亮)+ 选中动作。
/// `view` 是 `rows` 的过滤下标,`sel` 是 `view` 内位次;`editing=Some` = 配置页正编辑选中键值。
pub(crate) struct Panel {
    pub(crate) kind: PanelKind,
    pub(crate) title: String,
    pub(crate) query: String,
    pub(crate) rows: Vec<PanelRow>,
    pub(crate) view: Vec<usize>,
    pub(crate) sel: usize,
    pub(crate) editing: Option<String>,
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
        Panel {
            kind,
            title,
            query: String::new(),
            rows,
            view,
            sel: 0,
            editing: None,
        }
    }
    /// query 变更后重算 view 并把 sel 钳回范围内。
    pub(crate) fn retype(&mut self) {
        self.view = panel_filter(&self.rows, &self.query);
        if self.sel >= self.view.len() {
            self.sel = self.view.len().saturating_sub(1);
        }
    }
    pub(crate) fn move_up(&mut self) {
        self.sel = self.sel.saturating_sub(1);
    }
    pub(crate) fn move_down(&mut self) {
        if self.sel + 1 < self.view.len() {
            self.sel += 1;
        }
    }
    pub(crate) fn selected(&self) -> Option<&PanelRow> {
        self.view.get(self.sel).map(|&i| &self.rows[i])
    }
}

/// 当前 config 某标量键的显示值(空 → 空串,由调用方替 `(未设)`)。
pub(crate) fn config_value(cfg: &Config, key: &str) -> String {
    match key {
        "provider" => cfg.provider.clone().unwrap_or_default(),
        "model" => cfg.model.clone().unwrap_or_default(),
        "base_url" => cfg.base_url.clone().unwrap_or_default(),
        "budget_tokens" => cfg.budget_tokens.map(|n| n.to_string()).unwrap_or_default(),
        "skills_dir" => cfg.skills_dir.clone().unwrap_or_default(),
        "skip_danger" => cfg.skip_danger.map(|b| b.to_string()).unwrap_or_default(),
        "status_bar" => cfg.status_bar.clone().unwrap_or_default(),
        "allow_jailbreak" => cfg
            .allow_jailbreak
            .map(|b| b.to_string())
            .unwrap_or_default(),
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

/// Provider 页:列命名档(名 · kind · model)。
pub(crate) fn provider_panel() -> Panel {
    let cfg = Config::load(config_path());
    let rows = cfg
        .providers
        .iter()
        .map(|p| PanelRow {
            key: p.name.clone(),
            value: format!("{} · {}", p.kind, p.model),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Provider,
        "Provider · ↑↓ select · Enter switch · Esc close".into(),
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

/// 模型页:列实时模型(id · ctx),`sel` 落当前模型;`ctx` 携真实窗口供选中缓存。
pub(crate) fn models_panel(list: &[provider::models::ModelInfo], current: &str) -> Panel {
    let rows = list
        .iter()
        .map(|m| PanelRow {
            key: m.id.clone(),
            value: format!(
                "ctx {}",
                m.context.map(fmt_ctx).unwrap_or_else(|| "?".into())
            ),
            ctx: m.context,
        })
        .collect();
    let mut p = Panel::new(
        PanelKind::Models,
        "Models · ↑↓ select · Enter switch · type to filter · Esc close".into(),
        rows,
    );
    if let Some(pos) = p.view.iter().position(|&i| p.rows[i].key == current) {
        p.sel = pos;
    }
    p
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

/// 登录页(iter-38):列 14 家内置供应商 preset(id · label · model),Enter 选中后就地输入 key。
pub(crate) fn login_panel() -> Panel {
    let rows = PROVIDER_PRESETS
        .iter()
        .map(|p| PanelRow {
            key: p.id.to_string(),
            value: format!("{} · {}", p.label, p.default_model),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Login,
        "Login · ↑↓ pick provider · Enter enter key · type to filter · Esc close".into(),
        rows,
    )
}

/// 交互页键路由(iter-35,纯函数):模态优先级 审批 > Panel > 浮窗 > 输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelAction {
    Up,
    Down,
    Enter,
    Esc,
    Char(char),
    Backspace,
    Ignore,
}

pub(crate) fn panel_action(key: &KeyEvent) -> PanelAction {
    if key.kind != KeyEventKind::Press {
        return PanelAction::Ignore;
    }
    match key.code {
        KeyCode::Up => PanelAction::Up,
        KeyCode::Down => PanelAction::Down,
        KeyCode::Enter => PanelAction::Enter,
        KeyCode::Esc => PanelAction::Esc,
        KeyCode::Backspace => PanelAction::Backspace,
        KeyCode::Char(c) => PanelAction::Char(c),
        _ => PanelAction::Ignore,
    }
}
