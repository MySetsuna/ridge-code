//! 交互式主界面：用户直接运行 `ridgecode` 进入，在其中配置 providers、输入任务并执行。

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event as CtEvent, KeyCode, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::catalog;
use crate::config_ui;
use crate::input::TextArea;
use crate::Config;
use std::time::Duration;

const POLL_MS: u64 = 80;

/// 交互模式的状态。
#[derive(PartialEq, Clone, Copy)]
enum AppState {
    /// 主界面，等待用户输入。
    Main,
    /// 配置界面。
    Config,
    /// 执行任务中（会切换到 TUI）。
    Running,
    /// 退出。
    Exit,
}

/// 运行交互式主界面。
pub async fn run_interactive(config: Config, cwd: Option<std::path::PathBuf>) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut state = AppState::Main;
    let mut text_area = TextArea::new();
    let mut config = config;
    let mut status_message: Option<String> = None;

    loop {
        // 渲染
        terminal.draw(|f| {
            ui(f, &state, &text_area, &config, &status_message);
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
                // Ctrl+C 或 Ctrl+D 退出
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    state = AppState::Exit;
                }
                if key.code == KeyCode::Char('d')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    state = AppState::Exit;
                }

                match state {
                    AppState::Main => {
                        text_area.handle_key(key);

                        // 检查当前输入是否以 / 开头，显示命令建议
                        let current_input = text_area.get_text();
                        if current_input.starts_with('/') && !current_input.contains(' ') {
                            status_message = Some(get_command_hint(&current_input));
                        } else if !current_input.starts_with('/') {
                            status_message = None;
                        }

                        // 检查是否提交了任务
                        if let Some(task) = text_area.take_submitted() {
                            if task.starts_with('/') {
                                // 处理命令
                                match handle_command(&task, &mut config, &cwd).await {
                                    CommandResult::Config => {
                                        state = AppState::Config;
                                    }
                                    CommandResult::Help(msg) => {
                                        status_message = Some(msg);
                                    }
                                    CommandResult::Providers(msg) => {
                                        status_message = Some(msg);
                                    }
                                    CommandResult::Models(msg) => {
                                        status_message = Some(msg);
                                    }
                                    CommandResult::Exit => {
                                        state = AppState::Exit;
                                    }
                                    CommandResult::None => {}
                                }
                            } else if !task.is_empty() {
                                // 执行任务
                                ratatui::restore();
                                let result =
                                    execute_task(task, config.clone(), cwd.clone()).await;
                                // 重新初始化 terminal
                                terminal = ratatui::init();
                                match result {
                                    Ok(msg) => {
                                        status_message = Some(msg);
                                    }
                                    Err(e) => {
                                        status_message = Some(format!("错误: {}", e));
                                    }
                                }
                            }
                            text_area.reset();
                        }
                    }
                    AppState::Config => {
                        // 进入配置界面
                        ratatui::restore();
                        let saved = config_ui::run_config_ui(&mut config).await?;
                        // 重新初始化 terminal
                        terminal = ratatui::init();
                        if saved {
                            status_message = Some("配置已保存".to_string());
                        }
                        state = AppState::Main;
                    }
                    AppState::Running | AppState::Exit => {}
                }
            }
        }

        if state == AppState::Exit {
            break;
        }
    }

    ratatui::restore();
    Ok(())
}

/// 命令执行结果。
enum CommandResult {
    Config,
    Help(String),
    Providers(String),
    Models(String),
    Exit,
    None,
}

/// 处理斜杠命令。
async fn handle_command(
    cmd: &str,
    config: &mut Config,
    _cwd: &Option<std::path::PathBuf>,
) -> CommandResult {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    match parts.first().map(|s| *s) {
        Some("/help") | Some("/h") | Some("?") => {
            CommandResult::Help(
                "可用命令:\n\
                 /model          - 查看/切换当前模型\n\
                 /mcp            - 管理 MCP 服务器\n\
                 /config, /c     - 配置 providers 和 models\n\
                 /providers, /p  - 查看内置供应商列表\n\
                 /models, /m     - 查看模型列表\n\
                 /init           - 初始化配置文件\n\
                 /cwd            - 查看/切换工作目录\n\
                 /help, /h, ?    - 显示此帮助\n\
                 /quit, /q       - 退出程序"
                    .to_string(),
            )
        }
        Some("/config") | Some("/c") => CommandResult::Config,
        Some("/model") | Some("/mod") => {
            let subcmd = parts.get(1).copied();
            match subcmd {
                None => {
                    let mut buf = String::from("当前模型配置:\n\n");
                    if !config.providers.is_empty() {
                        for p in &config.providers {
                            let name = p.name.as_deref().unwrap_or("(unnamed)");
                            let role = if config.roles.strong.as_deref() == Some(name) {
                                " [Strong]"
                            } else if config.roles.weak.as_deref() == Some(name) {
                                " [Weak]"
                            } else {
                                ""
                            };
                            buf.push_str(&format!("  {} - {}{}\n", name, p.model, role));
                        }
                    }
                    buf.push_str("\n用法:\n");
                    buf.push_str("  /model list                - 获取可用模型列表\n");
                    buf.push_str("  /model <provider> <model>  - 设置 Strong 模型\n");
                    buf.push_str("  /model weak <provider> <model> - 设置 Weak 模型\n");
                    buf.push_str("  /model strong <provider> <model> - 设置 Strong 模型");
                    CommandResult::Help(buf)
                }
                Some("list") | Some("ls") => {
                    let mut buf = String::from("可用模型:\n\n");
                    for p in &config.providers {
                        let name = p.name.as_deref().unwrap_or("unnamed");
                        buf.push_str(&format!("[{}] {}:\n", name, p.base_url));
                        match fetch_models_from_api(&p.base_url).await {
                            Ok(models) => {
                                for m in &models {
                                    let marker = if p.model == *m { " *" } else { "" };
                                    buf.push_str(&format!("  {}{}\n", m, marker));
                                }
                            }
                            Err(e) => buf.push_str(&format!("  获取失败: {}\n", e)),
                        }
                        buf.push('\n');
                    }
                    if config.providers.is_empty() {
                        buf.push_str("未配置 provider，请先用 /config 添加");
                    }
                    buf.push_str("\n* = 当前使用");
                    CommandResult::Help(buf)
                }
                Some("strong") => {
                    let provider_name = parts.get(2).copied().unwrap_or("");
                    let model_name = parts.get(3).copied().unwrap_or("");
                    if provider_name.is_empty() || model_name.is_empty() {
                        return CommandResult::Help("用法: /model strong <provider> <model>".to_string());
                    }
                    set_model_config(config, provider_name, model_name, true)
                }
                Some("weak") => {
                    let provider_name = parts.get(2).copied().unwrap_or("");
                    let model_name = parts.get(3).copied().unwrap_or("");
                    if provider_name.is_empty() || model_name.is_empty() {
                        return CommandResult::Help("用法: /model weak <provider> <model>".to_string());
                    }
                    set_model_config(config, provider_name, model_name, false)
                }
                Some(provider_name) => {
                    let model_name = parts.get(2).copied().unwrap_or("");
                    if model_name.is_empty() {
                        return CommandResult::Help("用法: /model <provider> <model>".to_string());
                    }
                    set_model_config(config, provider_name, model_name, true)
                }
            }
        }
        Some("/mcp") => {
            let subcmd = parts.get(1).copied();
            match subcmd {
                None | Some("list") | Some("ls") => {
                    let mut buf = String::from("MCP 服务器:\n\n");
                    if config.mcp.is_empty() {
                        buf.push_str("  (未配置)\n");
                    } else {
                        for m in &config.mcp {
                            buf.push_str(&format!("  {} - {} {:?}\n", m.name, m.command, m.args));
                        }
                    }
                    buf.push_str("\n用法:\n");
                    buf.push_str("  /mcp add <name> <command> [args...]  - 添加 MCP 服务器\n");
                    buf.push_str("  /mcp remove <name>                    - 移除 MCP 服务器\n");
                    buf.push_str("  /mcp edit                             - 编辑配置文件");
                    CommandResult::Help(buf)
                }
                Some("add") => {
                    let name = parts.get(2).copied().unwrap_or("");
                    let command = parts.get(3).copied().unwrap_or("");
                    if name.is_empty() || command.is_empty() {
                        return CommandResult::Help("用法: /mcp add <name> <command> [args...]".to_string());
                    }
                    let args: Vec<String> = parts[4..].iter().map(|s| s.to_string()).collect();
                    config.mcp.push(crate::McpServerConfig {
                        name: name.to_string(),
                        command: command.to_string(),
                        args,
                        env: std::collections::HashMap::new(),
                    });
                    CommandResult::Help(format!("已添加 MCP 服务器: {}", name))
                }
                Some("remove") | Some("rm") => {
                    let name = parts.get(2).copied().unwrap_or("");
                    if name.is_empty() {
                        return CommandResult::Help("用法: /mcp remove <name>".to_string());
                    }
                    let before = config.mcp.len();
                    config.mcp.retain(|m| m.name != name);
                    if config.mcp.len() < before {
                        CommandResult::Help(format!("已移除 MCP 服务器: {}", name))
                    } else {
                        CommandResult::Help(format!("未找到 MCP 服务器: {}", name))
                    }
                }
                _ => CommandResult::Help("用法: /mcp [list|add|remove]".to_string()),
            }
        }
        Some("/providers") | Some("/p") => {
            let mut buf = String::from("内置供应商:\n\n");
            buf.push_str(&format!(
                "  {:<11} {:<10} {:<5} {:<46}\n",
                "NAME", "KIND", "FREE", "BASE_URL"
            ));
            for e in catalog::catalog() {
                let free = if e.free { "✓" } else { "" };
                buf.push_str(&format!(
                    "  {:<11} {:<10} {:<5} {}\n",
                    e.name,
                    e.kind.as_str(),
                    free,
                    e.base_url
                ));
            }
            buf.push_str("\n用 /init <provider> [model] 生成配置");
            CommandResult::Providers(buf)
        }
        Some("/models") | Some("/m") => {
            let provider_name = parts.get(1).map(|s| *s);
            let mut buf = String::from("内置示例模型:\n\n");
            let entries: Vec<&catalog::CatalogEntry> = match provider_name {
                Some(name) => vec![match catalog::find(name) {
                    Some(e) => e,
                    None => {
                        return CommandResult::Models(format!("未知供应商: {}", name));
                    }
                }],
                None => catalog::catalog().iter().collect(),
            };
            for e in entries {
                buf.push_str(&format!("[{}] {}\n", e.name, e.note));
                for m in e.models {
                    buf.push_str(&format!("  - {}\n", m));
                }
                buf.push('\n');
            }
            CommandResult::Models(buf)
        }
        Some("/init") => {
            let provider = match parts.get(1) {
                Some(p) => *p,
                None => {
                    return CommandResult::Help(
                        "用法: /init <provider> [model]\n\
                         例: /init deepseek"
                            .to_string(),
                    );
                }
            };
            let model = parts.get(2).map(|s| s.to_string());
            match crate::cmd_init_interactive(provider, model) {
                Ok(msg) => CommandResult::Help(msg),
                Err(e) => CommandResult::Help(format!("初始化失败: {}", e)),
            }
        }
        Some("/cwd") => {
            if let Some(dir) = parts.get(1) {
                CommandResult::Help(format!("工作目录: {}", dir))
            } else {
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "未知".to_string());
                CommandResult::Help(format!("当前工作目录: {}\n用 /cwd <path> 切换", cwd))
            }
        }
        Some("/quit") | Some("/q") => CommandResult::Exit,
        _ => CommandResult::Help(format!(
            "未知命令: {}\n输入 /help 查看可用命令",
            cmd
        )),
    }
}

/// 设置模型配置。
fn set_model_config(config: &mut Config, provider_name: &str, model_name: &str, is_strong: bool) -> CommandResult {
    let role_value = format!("{},{}", provider_name, model_name);

    // 检查 provider 是否已配置，没有则添加
    let exists = config.providers.iter().any(|p| p.name.as_deref() == Some(provider_name));
    if !exists {
        // 尝试从内置目录获取 provider 信息
        match crate::catalog::find(provider_name) {
            Some(entry) => {
                config.providers.push(crate::ProviderConfig {
                    name: Some(provider_name.to_string()),
                    kind: entry.kind,
                    base_url: entry.base_url.to_string(),
                    model: model_name.to_string(),
                    api_key: None,
                    api_key_env: Some(entry.api_key_env.to_string()),
                    max_tokens: None,
                });
            }
            None => {
                return CommandResult::Help(format!("未知供应商: {}，请先用 /config 添加", provider_name));
            }
        }
    } else {
        // 更新现有 provider 的模型
        if let Some(p) = config.providers.iter_mut().find(|p| p.name.as_deref() == Some(provider_name)) {
            p.model = model_name.to_string();
        }
    }

    // 设置角色
    if is_strong {
        config.roles.strong = Some(role_value.clone());
        CommandResult::Help(format!("Strong 模型已设置为: {}", role_value))
    } else {
        config.roles.weak = Some(role_value.clone());
        CommandResult::Help(format!("Weak 模型已设置为: {}", role_value))
    }
}

/// 从 OpenAI 兼容 API 获取模型列表。
async fn fetch_models_from_api(base_url: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    #[derive(serde::Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelItem>,
    }

    #[derive(serde::Deserialize)]
    struct ModelItem {
        id: String,
    }

    let body: ModelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("解析响应失败: {}", e))?;

    let mut models: Vec<String> = body.data.into_iter().map(|m| m.id).collect();
    models.sort();
    Ok(models)
}

/// 执行任务（调用现有的 Orchestrator 逻辑）。
async fn execute_task(
    task: String,
    config: Config,
    cwd: Option<std::path::PathBuf>,
) -> Result<String> {
    use rc_core::{Orchestrator, OrchestratorConfig};
    use rc_mcp::McpHub;

    // 切换工作目录
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .with_context(|| format!("切换目录到 {} 失败", dir.display()))?;
    }
    let project_dir = std::env::current_dir().context("获取当前目录失败")?;

    // 构建 providers
    let registry = crate::build_registry(&config.providers);
    let (strong_cfg, weak_cfg) = crate::resolve_tiers(&config, &registry)?;
    let worker_models = crate::resolve_worker_models(&config, &registry)?;

    let _strong_model = strong_cfg.model.clone();
    let _weak_model = weak_cfg.model.clone();
    let strong = crate::build_provider(&strong_cfg)?;
    let weak = crate::build_provider(&weak_cfg)?;

    // 构建 Orchestrator
    let mut orch = Orchestrator::new(
        strong,
        weak,
        project_dir,
        OrchestratorConfig {
            max_steps: 12,
            max_repairs: 3,
        },
    );

    if !worker_models.is_empty() {
        orch = orch.with_worker_models(worker_models);
    }

    // MCP
    if !config.mcp.is_empty() {
        let hub = McpHub::connect(config.mcp).await;
        orch = orch.with_mcp(hub);
    }

    // 执行（使用 TUI 显示进度）
    let outcome = crate::tui::run_with_tui(orch, task).await?;

    // 如果是对话回复，直接返回
    if let Some(reply) = outcome.reply {
        return Ok(reply);
    }

    // 生成报告
    let c = &outcome.cost;
    let review_status = if outcome.reviewed {
        if outcome.approved {
            "通过"
        } else {
            "未通过"
        }
    } else {
        "跳过"
    };

    Ok(format!(
        "完成! 子任务: {}  修复轮次: {}  评审: {}\n\
         Token  强: {}  弱: {}  强占比: {:.0}%",
        outcome.subtasks,
        outcome.repairs,
        review_status,
        c.strong_tokens(),
        c.weak_tokens(),
        c.strong_share() * 100.0
    ))
}

/// 渲染主界面。
fn ui(
    frame: &mut Frame,
    _state: &AppState,
    text_area: &TextArea,
    config: &Config,
    status: &Option<String>,
) {
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // 获取当前工作目录和 git 分支
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "未知".to_string());
    let git_branch = get_git_branch();

    // 顶部：标题 + 工作目录 + 分支
    let header_line = Line::from(vec![
        Span::styled(
            " ridge-code ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            &cwd,
            Style::default().fg(Color::Yellow),
        ),
        if !git_branch.is_empty() {
            Span::styled(
                format!(" ({})", git_branch),
                Style::default().fg(Color::Green),
            )
        } else {
            Span::raw("")
        },
        Span::styled(
            "  输入任务或 /help",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header_line).block(Block::bordered()),
        header,
    );

    // 中间区域
    let [config_area, input_area] =
        Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(main);

    // 左侧：当前配置
    render_config_summary(frame, config, config_area);

    // 右侧：输入框
    text_area.render(frame, input_area, " 输入任务 ");

    // 底部：状态/帮助信息
    let foot = if let Some(msg) = status {
        msg.as_str()
    } else {
        "Enter 提交 | Shift+Enter 换行 | Ctrl+C 退出 | /help 帮助"
    };
    frame.render_widget(
        Paragraph::new(foot).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

/// 渲染配置摘要。
fn render_config_summary(frame: &mut Frame, config: &Config, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        " 当前配置",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));

    // 显示当前使用的模型 (支持 "provider,model" 格式)
    let format_role = |role: &Option<String>| -> String {
        role.as_ref().map(|s| s.as_str()).unwrap_or("(未配置)").to_string()
    };

    lines.push(Line::from(vec![
        Span::styled(" Strong: ", Style::default().fg(Color::Magenta)),
        Span::raw(format_role(&config.roles.strong)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" Weak:   ", Style::default().fg(Color::Cyan)),
        Span::raw(format_role(&config.roles.weak)),
    ]));

    // 显示已配置的 providers 数量
    if !config.providers.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(" Providers: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!("{} 个已配置", config.providers.len())),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " 命令: /model 切换模型 | /config 编辑配置",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" 配置摘要 ")),
        area,
    );
}

/// 截断字符串。
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}…", truncated)
    }
}

/// 根据输入前缀返回命令提示
fn get_command_hint(input: &str) -> String {
    let commands = [
        ("/model", "切换当前模型"),
        ("/mcp", "管理 MCP 服务器"),
        ("/config", "配置 providers 和 models"),
        ("/providers", "查看内置供应商列表"),
        ("/models", "查看模型列表"),
        ("/init", "初始化配置文件"),
        ("/cwd", "查看/切换工作目录"),
        ("/help", "显示帮助"),
        ("/quit", "退出程序"),
    ];

    let prefix = input.trim_start_matches('/');
    if prefix.is_empty() {
        // 显示所有命令
        let list: Vec<String> = commands
            .iter()
            .map(|(cmd, desc)| format!("  {} - {}", cmd, desc))
            .collect();
        return format!("可用命令:\n{}", list.join("\n"));
    }

    // 过滤匹配的命令
    let matches: Vec<&str> = commands
        .iter()
        .filter(|(cmd, _)| cmd[1..].starts_with(prefix))
        .map(|(cmd, _)| *cmd)
        .collect();

    if matches.is_empty() {
        format!("无匹配命令: /{}", prefix)
    } else if matches.len() == 1 {
        // 精确匹配一个，显示完整帮助
        let cmd = matches[0];
        let desc = commands.iter().find(|(c, _)| *c == cmd).unwrap().1;
        format!("{} - {}  (Enter 执行)", cmd, desc)
    } else {
        // 多个匹配，显示列表
        let list: Vec<String> = matches
            .iter()
            .map(|cmd| {
                let desc = commands.iter().find(|(c, _)| *c == *cmd).unwrap().1;
                format!("  {} - {}", cmd, desc)
            })
            .collect();
        format!("匹配的命令:\n{}", list.join("\n"))
    }
}

/// 获取当前 git 分支名
fn get_git_branch() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}
