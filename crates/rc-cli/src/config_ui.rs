//! 配置管理界面：完整可视化配置 providers、roles、routing。

use anyhow::{anyhow, Result};
use ratatui::crossterm::event::{self, Event as CtEvent, KeyCode, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::input::TextArea;
use crate::Config;
use crate::ProviderConfig;
use crate::ProviderKind;

use std::time::Duration;

const POLL_MS: u64 = 80;

/// 配置界面的视图状态。
#[derive(PartialEq, Clone, Copy)]
enum ConfigView {
    /// 主配置视图：Provider 列表 + Roles。
    Main,
    /// 编辑 Provider 视图。
    EditProvider,
    /// 确认删除对话框。
    ConfirmDelete,
}

/// 配置界面的焦点。
#[derive(PartialEq, Clone, Copy)]
enum Focus {
    ProviderList,
    Roles,
    Actions,
}

/// 编辑 Provider 时的焦点。
#[derive(PartialEq, Clone, Copy)]
enum EditField {
    Name,
    Kind,
    BaseUrl,
    Model,
    ApiKey,
    ApiKeyEnv,
}

/// 运行配置界面。
/// 返回 Ok(true) 表示已保存，Ok(false) 表示放弃。
pub async fn run_config_ui(config: &mut Config) -> Result<bool> {
    let mut terminal = ratatui::init();
    let result = run_config_loop(&mut terminal, config);
    ratatui::restore();
    result
}

/// 配置界面主循环。
fn run_config_loop(
    terminal: &mut ratatui::DefaultTerminal,
    config: &mut Config,
) -> Result<bool> {
    // 克隆一份用于取消时恢复
    let original_config = config.clone();

    let mut view = ConfigView::Main;
    let mut focus = Focus::ProviderList;
    let mut selected_provider = 0;
    let mut _list_state = ListState::default();

    // 编辑状态
    let mut edit_field = EditField::Name;
    let mut edit_text = TextArea::new();
    let mut edit_index: Option<usize> = None; // None = 添加新 provider

    // 删除确认
    let mut delete_index: usize = 0;

    // 角色编辑
    let mut editing_role: Option<String> = None; // "strong" 或 "weak"

    // 状态消息
    let mut status_msg: Option<String> = None;

    loop {
        // 确保选中索引有效
        if !config.providers.is_empty() {
            selected_provider = selected_provider.min(config.providers.len() - 1);
        }
        _list_state.select(Some(selected_provider));

        // 渲染
        terminal.draw(|f| {
            ui(f, config, &view, &focus, &_list_state, &edit_text, &editing_role, &status_msg);
        })?;

        // 处理事件
        if event::poll(Duration::from_millis(POLL_MS))? {
            if let CtEvent::Key(key) = event::read()? {
                // 只处理按键按下事件，忽略释放和重复
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // 过滤掉 Windows 上的额外状态事件
                if key.state != KeyEventState::NONE && key.state != KeyEventState::CAPS_LOCK {
                    continue;
                }
                // Esc 返回
                if key.code == KeyCode::Esc {
                    match view {
                        ConfigView::Main => {
                            // 退出配置界面
                            *config = original_config;
                            return Ok(false);
                        }
                        ConfigView::EditProvider => {
                            view = ConfigView::Main;
                            edit_text.reset();
                        }
                        ConfigView::ConfirmDelete => {
                            view = ConfigView::Main;
                        }
                    }
                    continue;
                }

                // Ctrl+S 保存
                if key.code == KeyCode::Char('s')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    save_config(config)?;
                    return Ok(true);
                }

                match view {
                    ConfigView::Main => handle_main_view(
                        key,
                        config,
                        &mut view,
                        &mut focus,
                        &mut selected_provider,
                        &mut edit_text,
                        &mut edit_index,
                        &mut delete_index,
                        &mut editing_role,
                        &mut status_msg,
                    )?,
                    ConfigView::EditProvider => handle_edit_view(
                        key,
                        config,
                        &mut view,
                        &mut edit_field,
                        &mut edit_text,
                        &mut edit_index,
                        &mut status_msg,
                    )?,
                    ConfigView::ConfirmDelete => handle_delete_confirm(
                        key,
                        config,
                        &mut view,
                        selected_provider,
                        &mut status_msg,
                    )?,
                }
            }
        }
    }
}

/// 处理主视图的键盘事件。
fn handle_main_view(
    key: ratatui::crossterm::event::KeyEvent,
    config: &mut Config,
    view: &mut ConfigView,
    focus: &mut Focus,
    selected: &mut usize,
    edit_text: &mut TextArea,
    edit_index: &mut Option<usize>,
    delete_index: &mut usize,
    editing_role: &mut Option<String>,
    status_msg: &mut Option<String>,
) -> Result<()> {
    match key.code {
        // Tab 切换焦点
        KeyCode::Tab => {
            *focus = match *focus {
                Focus::ProviderList => Focus::Roles,
                Focus::Roles => Focus::Actions,
                Focus::Actions => Focus::ProviderList,
            };
        }
        // 上下移动
        KeyCode::Up | KeyCode::Char('k') => {
            match *focus {
                Focus::ProviderList => {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
                Focus::Roles => {
                    // 切换 strong/weak 编辑
                    *editing_role = Some(match editing_role.as_deref() {
                        Some("weak") => "strong".to_string(),
                        _ => "weak".to_string(),
                    });
                }
                Focus::Actions => {}
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            match *focus {
                Focus::ProviderList => {
                    if !config.providers.is_empty() && *selected < config.providers.len() - 1 {
                        *selected += 1;
                    }
                }
                Focus::Roles => {
                    *editing_role = Some(match editing_role.as_deref() {
                        Some("strong") => "weak".to_string(),
                        _ => "strong".to_string(),
                    });
                }
                Focus::Actions => {}
            }
        }
        // Enter 操作
        KeyCode::Enter => {
            match *focus {
                Focus::ProviderList => {
                    if !config.providers.is_empty() {
                        // 编辑选中的 provider
                        *edit_index = Some(*selected);
                        let p = &config.providers[*selected];
                        edit_text.set_text(p.name.clone().unwrap_or_default());
                        *view = ConfigView::EditProvider;
                    }
                }
                Focus::Roles => {
                    // 开始编辑角色
                    match editing_role.as_deref() {
                        Some("strong") => {
                            let current = config.roles.strong.clone().unwrap_or_default();
                            edit_text.set_text(current);
                        }
                        Some("weak") => {
                            let current = config.roles.weak.clone().unwrap_or_default();
                            edit_text.set_text(current);
                        }
                        _ => {
                            *editing_role = Some("strong".to_string());
                            let current = config.roles.strong.clone().unwrap_or_default();
                            edit_text.set_text(current);
                        }
                    }
                }
                Focus::Actions => {
                    // 根据当前选中的操作执行
                    // 这里简化处理，用快捷键代替
                }
            }
        }
        // 添加 provider
        KeyCode::Char('a') if *focus == Focus::ProviderList => {
            *edit_index = None;
            edit_text.set_text(String::new());
            *view = ConfigView::EditProvider;
        }
        // 删除 provider
        KeyCode::Char('d') if *focus == Focus::ProviderList => {
            if !config.providers.is_empty() {
                *delete_index = *selected;
                *view = ConfigView::ConfirmDelete;
            }
        }
        // 快捷键切换角色
        KeyCode::Char('1') => {
            if let Some(p) = config.providers.get(*selected) {
                config.roles.strong = p.name.clone();
                *status_msg = Some(format!("Strong 设为 {}", p.name.as_deref().unwrap_or("unnamed")));
            }
        }
        KeyCode::Char('2') => {
            if let Some(p) = config.providers.get(*selected) {
                config.roles.weak = p.name.clone();
                *status_msg = Some(format!("Weak 设为 {}", p.name.as_deref().unwrap_or("unnamed")));
            }
        }
        _ => {}
    }
    Ok(())
}

/// 处理编辑 Provider 视图的键盘事件。
fn handle_edit_view(
    key: ratatui::crossterm::event::KeyEvent,
    config: &mut Config,
    view: &mut ConfigView,
    field: &mut EditField,
    edit_text: &mut TextArea,
    edit_index: &mut Option<usize>,
    status_msg: &mut Option<String>,
) -> Result<()> {
    match key.code {
        // Tab 切换字段
        KeyCode::Tab => {
            // 保存当前字段
            save_current_field(config, field, edit_text, *edit_index)?;
            // 切换到下一个字段
            *field = match *field {
                EditField::Name => EditField::Kind,
                EditField::Kind => EditField::BaseUrl,
                EditField::BaseUrl => EditField::Model,
                EditField::Model => EditField::ApiKey,
                EditField::ApiKey => EditField::ApiKeyEnv,
                EditField::ApiKeyEnv => EditField::Name,
            };
            // 加载新字段的值
            load_field_value(config, field, edit_text, *edit_index);
        }
        // Enter 提交并返回
        KeyCode::Enter => {
            save_current_field(config, field, edit_text, *edit_index)?;
            *view = ConfigView::Main;
            edit_text.reset();
            *status_msg = Some("Provider 已保存".to_string());
        }
        // 空格在 Kind 字段时切换
        KeyCode::Char(' ') if *field == EditField::Kind => {
            toggle_kind(config, *edit_index);
        }
        // 其他输入交给 TextArea
        _ => {
            edit_text.handle_key(key);
        }
    }
    Ok(())
}

/// 保存当前字段的值到 config。
fn save_current_field(
    config: &mut Config,
    field: &EditField,
    edit_text: &TextArea,
    edit_index: Option<usize>,
) -> Result<()> {
    let value = edit_text.get_text();

    match *field {
        EditField::Name => {
            let name = if value.is_empty() { None } else { Some(value) };
            match edit_index {
                Some(idx) => {
                    config.providers[idx].name = name;
                }
                None => {
                    // 添加新 provider
                    config.providers.push(ProviderConfig {
                        name,
                        kind: ProviderKind::Openai,
                        base_url: String::new(),
                        model: String::new(),
                        api_key: None,
                        api_key_env: None,
                        max_tokens: None,
                    });
                }
            }
        }
        EditField::BaseUrl => {
            if let Some(idx) = edit_index {
                config.providers[idx].base_url = value;
            } else if let Some(idx) = config.providers.last().map(|_| config.providers.len() - 1) {
                config.providers[idx].base_url = value;
            }
        }
        EditField::Model => {
            if let Some(idx) = edit_index {
                config.providers[idx].model = value;
            } else if let Some(idx) = config.providers.last().map(|_| config.providers.len() - 1) {
                config.providers[idx].model = value;
            }
        }
        EditField::ApiKey => {
            let key = if value.is_empty() { None } else { Some(value) };
            if let Some(idx) = edit_index {
                config.providers[idx].api_key = key;
            } else if let Some(idx) = config.providers.last().map(|_| config.providers.len() - 1) {
                config.providers[idx].api_key = key;
            }
        }
        EditField::ApiKeyEnv => {
            let env = if value.is_empty() { None } else { Some(value) };
            if let Some(idx) = edit_index {
                config.providers[idx].api_key_env = env;
            } else if let Some(idx) = config.providers.last().map(|_| config.providers.len() - 1) {
                config.providers[idx].api_key_env = env;
            }
        }
        EditField::Kind => {
            // Kind 通过空格切换，不需要从输入框保存
        }
    }
    Ok(())
}

/// 加载字段值到编辑框。
fn load_field_value(config: &Config, field: &EditField, edit_text: &mut TextArea, edit_index: Option<usize>) {
    let get_value = |p: &ProviderConfig| -> String {
        match *field {
            EditField::Name => p.name.clone().unwrap_or_default(),
            EditField::BaseUrl => p.base_url.clone(),
            EditField::Model => p.model.clone(),
            EditField::ApiKey => p.api_key.clone().unwrap_or_default(),
            EditField::ApiKeyEnv => p.api_key_env.clone().unwrap_or_default(),
            EditField::Kind => p.kind.as_str().to_string(),
        }
    };

    let value = match edit_index {
        Some(idx) => config.providers.get(idx).map(get_value).unwrap_or_default(),
        None => String::new(),
    };
    edit_text.set_text(value);
}

/// 切换 Kind。
fn toggle_kind(config: &mut Config, edit_index: Option<usize>) {
    if let Some(idx) = edit_index {
        config.providers[idx].kind = match config.providers[idx].kind {
            ProviderKind::Openai => ProviderKind::Anthropic,
            ProviderKind::Anthropic => ProviderKind::Openai,
        };
    }
}

/// 处理删除确认视图。
fn handle_delete_confirm(
    key: ratatui::crossterm::event::KeyEvent,
    config: &mut Config,
    view: &mut ConfigView,
    delete_index: usize,
    status_msg: &mut Option<String>,
) -> Result<()> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if delete_index < config.providers.len() {
                config.providers.remove(delete_index);
                *status_msg = Some("Provider 已删除".to_string());
            }
            *view = ConfigView::Main;
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            *view = ConfigView::Main;
        }
        _ => {}
    }
    Ok(())
}

/// 保存配置到文件。
fn save_config(config: &Config) -> Result<()> {
    let home = home_dir()
        .ok_or_else(|| anyhow!("找不到 HOME / USERPROFILE"))?;
    let dir = home.join(".ridge");
    let path = dir.join("config.toml");

    let toml = toml::to_string_pretty(config)
        .map_err(|e| anyhow!("序列化配置失败: {}", e))?;

    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow!("创建目录 {} 失败: {}", dir.display(), e))?;
    std::fs::write(&path, &toml)
        .map_err(|e| anyhow!("写入 {} 失败: {}", path.display(), e))?;

    Ok(())
}

/// 获取用户主目录。
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// 渲染配置界面。
fn ui(
    frame: &mut Frame,
    config: &Config,
    view: &ConfigView,
    focus: &Focus,
    _list_state: &ListState,
    edit_text: &TextArea,
    editing_role: &Option<String>,
    status_msg: &Option<String>,
) {
    match view {
        ConfigView::Main => render_main_view(frame, config, focus, _list_state, editing_role, status_msg),
        ConfigView::EditProvider => render_edit_view(frame, config, edit_text, status_msg),
        ConfigView::ConfirmDelete => render_confirm_dialog(frame),
    }
}

/// 渲染主配置视图。
fn render_main_view(
    frame: &mut Frame,
    config: &Config,
    focus: &Focus,
    _list_state: &ListState,
    editing_role: &Option<String>,
    status_msg: &Option<String>,
) {
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // 标题
    let header_line = Line::from(vec![
        Span::styled(
            " ridge-code ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  配置管理 — Tab 切换区域 | Ctrl+S 保存 | Esc 返回"),
    ]);
    frame.render_widget(
        Paragraph::new(header_line).block(Block::bordered()),
        header,
    );

    // 左侧：Provider 列表
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(main);

    render_provider_list(frame, config, focus, _list_state, left);

    // 右侧：Roles + Routing
    render_roles_panel(frame, config, focus, editing_role, right);

    // 底部状态
    let foot = status_msg
        .as_deref()
        .unwrap_or("a 添加 | d 删除 | 1 设为 Strong | 2 设为 Weak | Enter 编辑");
    frame.render_widget(
        Paragraph::new(foot).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

/// 渲染 Provider 列表。
fn render_provider_list(
    frame: &mut Frame,
    config: &Config,
    focus: &Focus,
    _list_state: &ListState,
    area: Rect,
) {
    let border_style = if *focus == Focus::ProviderList {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let items: Vec<ListItem> = config
        .providers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = p.name.as_deref().unwrap_or("(unnamed)");
            let kind = p.kind.as_str();
            let url = truncate(&p.base_url, 30);

            // 标记是否是当前角色
            let is_strong = config.roles.strong.as_deref() == Some(name);
            let is_weak = config.roles.weak.as_deref() == Some(name);

            let mut spans = vec![
                Span::styled(
                    format!(" {:<3} ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:<12}", name),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("{:<10}", kind),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(url),
            ];

            if is_strong {
                spans.push(Span::styled(
                    " [Strong]",
                    Style::default().fg(Color::Magenta),
                ));
            }
            if is_weak {
                spans.push(Span::styled(
                    " [Weak]",
                    Style::default().fg(Color::Cyan),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    frame.render_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Providers ")
                    .border_style(border_style),
            )
            .highlight_style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

/// 渲染 Roles 面板。
fn render_roles_panel(
    frame: &mut Frame,
    config: &Config,
    focus: &Focus,
    editing_role: &Option<String>,
    area: Rect,
) {
    let border_style = if *focus == Focus::Roles {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        " Roles",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));

    // Strong role
    let strong_style = if editing_role.as_deref() == Some("strong") {
        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let strong_val = config.roles.strong.as_deref().unwrap_or("(未设置)");
    lines.push(Line::from(vec![
        Span::styled(" Strong: ", Style::default().fg(Color::Yellow)),
        Span::styled(strong_val.to_string(), strong_style),
    ]));

    // Weak role
    let weak_style = if editing_role.as_deref() == Some("weak") {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let weak_val = config.roles.weak.as_deref().unwrap_or("(未设置)");
    lines.push(Line::from(vec![
        Span::styled(" Weak:   ", Style::default().fg(Color::Yellow)),
        Span::styled(weak_val.to_string(), weak_style),
    ]));

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " 操作:",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(" Up/Down 选择角色"));
    lines.push(Line::raw(" 1 设选中为 Strong"));
    lines.push(Line::raw(" 2 设选中为 Weak"));

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 角色配置 ")
                .border_style(border_style),
        ),
        area,
    );
}

/// 渲染编辑 Provider 视图。
fn render_edit_view(
    frame: &mut Frame,
    _config: &Config,
    edit_text: &TextArea,
    status_msg: &Option<String>,
) {
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // 标题
    let header_line = Line::from(vec![
        Span::styled(
            " ridge-code ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  编辑 Provider — Tab 切换字段 | Enter 确认 | Esc 取消"),
    ]);
    frame.render_widget(
        Paragraph::new(header_line).block(Block::bordered()),
        header,
    );

    // 编辑区域
    let [fields_area, input_area] =
        Layout::vertical([Constraint::Length(12), Constraint::Min(0)]).areas(main);

    // 字段提示
    let fields = [
        ("Name", "Provider 名称（用于 roles 引用）"),
        ("Kind", "协议类型（空格切换: OpenAI / Anthropic）"),
        ("Base URL", "API 端点地址"),
        ("Model", "模型 ID"),
        ("API Key", "API 密钥（可留空，用环境变量）"),
        ("Env Var", "API Key 环境变量名"),
    ];

    let fields_lines: Vec<Line> = fields
        .iter()
        .map(|(name, desc)| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<10} ", name),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(desc.to_string()),
            ])
        })
        .collect();

    frame.render_widget(
        Paragraph::new(fields_lines).block(Block::bordered().title(" 字段说明 ")),
        fields_area,
    );

    // 输入框
    edit_text.render(frame, input_area, " 编辑值 ");

    // 底部
    let foot = status_msg
        .as_deref()
        .unwrap_or("Tab 下一字段 | Enter 保存 | Esc 取消");
    frame.render_widget(
        Paragraph::new(foot).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

/// 渲染确认删除对话框。
fn render_confirm_dialog(frame: &mut Frame) {
    let area = frame.area();
    let popup = ratatui::layout::Rect {
        x: area.width / 4,
        y: area.height / 3,
        width: area.width / 2,
        height: 5,
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::bordered().title(" 确认删除 "),
        popup,
    );

    let text = Line::from(vec![
        Span::raw("确认删除此 Provider? "),
        Span::styled("[Y] 确认", Style::default().fg(Color::Red)),
        Span::raw("  "),
        Span::styled("[N] 取消", Style::default().fg(Color::Green)),
    ]);

    frame.render_widget(
        Paragraph::new(text),
        Rect {
            x: popup.x + 1,
            y: popup.y + 1,
            width: popup.width - 2,
            height: popup.height - 2,
        },
    );
}

/// 截断字符串。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}…", truncated)
    }
}
