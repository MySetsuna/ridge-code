use crate::guard::*;
use crate::signals::{signal_create, signal_resolve, SIGNALS_DIR};
use crate::state::*;
use provider::{ToolCall, ToolSpec};

/// 内置工具的规格(喂给 LLM 让它按 schema 出结构化 tool_call)。
pub fn builtin_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "run_shell".to_string(),
            description: "运行一条 shell 命令,返回退出码与输出".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string"},"shell":{"type":"string","enum":["cmd","powershell","pwsh","bash","sh"],"description":"可选:执行用的 shell;省=宿主默认(见 host_env)"}},"required":["cmd"]}),
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "把内容整文件写入路径(覆盖)。仅用于**新建文件**;改动已有文件请用 edit_file".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"contents":{"type":"string"}},"required":["path","contents"]}),
        },
        ToolSpec {
            name: "edit_file".to_string(),
            description: "精准编辑:把文件里**唯一**出现的 old_string 换成 new_string(需带足够上下文保证唯一)。改动已有文件优先用它,而非整文件覆写".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}),
        },
        ToolSpec {
            name: "apply_edits".to_string(),
            description: "**跨文件批量**精准编辑:多处 {path, old_string, new_string} 汇总一份 diff 一次确认、**原子应用**(全成或全不改)。重构/多文件改动用它".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"edits":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}}},"required":["edits"]}),
        },
        ToolSpec {
            name: "read_file".to_string(),
            description: "读取文件。可选 offset(起始行,1 起)+ limit(行数)只读一段,大文件别整读".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}),
        },
        ToolSpec {
            name: "search".to_string(),
            description: "在目录树下按文件名 glob(如 *.rs)搜含 pattern 子串的行,返回 路径:行号:内容。找代码/定位用它,别 run_shell grep(不可移植)".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"}},"required":["pattern"]}),
        },
        ToolSpec {
            name: "web_search".to_string(),
            description: "联网搜索,返回标题/链接/摘要(自动按网络环境选可用引擎)。查实时信息或外部资料用它;query 会发给外部搜索引擎".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
        },
        ToolSpec {
            name: "fetch_url".to_string(),
            description: "抓取一个网页并返回**可读正文**(去脚本/样式/标签)。配合 web_search:先搜到链接,再用它读正文、据原文作答,别只凭摘要猜".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
        },
        ToolSpec {
            name: "todo_write".to_string(),
            description: "维护任务清单:把计划拆成若干 {content, status}。**多步/复杂任务**开始时列清单、每完成一步更新其状态给用户看进度;简单单步不必用".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["content","status"]}}},"required":["todos"]}),
        },
        ToolSpec {
            name: "signal_write".to_string(),
            description: "记录/消解**跨会话复用**的信号(发现/摩擦/待办)。记:给 type+body;消解已处理的:给 resolve=<id>。下个会话自动继承未决信号".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"type":{"type":"string"},"body":{"type":"string"},"resolve":{"type":"string"}}}),
        },
    ]
}

/// 从 `apply_edits` 的参数里抽出 `edits` 数组 → [`tools::Edit`] 列表(字段缺失→跳过)。
pub(crate) fn parse_edits(call: &ToolCall) -> Vec<tools::Edit> {
    let Some(arr) = call.arguments.get("edits").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| {
            let s = |k: &str| e.get(k).and_then(|v| v.as_str());
            Some(tools::Edit::new(
                s("path")?,
                s("old_string")?,
                s("new_string")?,
            ))
        })
        .collect()
}

/// 从一次工具调用 + 观察结果里,**确定性地**抽出 Durable State 更新(事实驱动回填):
/// 观察到工具错误(前缀 ` error:` / `BLOCKED` / `permission denied`)→ 置 `last_error` 首行;
/// 写类工具成功(write_file/edit_file/apply_edits)→ 记入 `modified_files` 并清 `last_error`;
/// 其余工具不动 durable 状态。这样长任务只凭「当前事实」推理,不必靠全量历史。
/// 工具观察是否为**错误**(` error:` / `BLOCKED` / `permission denied` / **非零 `exit N`**)。单一真相:
/// Durable State 回填与熔断计数(`err_streak`)共用,免两处判据漂移。**非零 exit 必判错**(iter-51):
/// 此前漏判 —— 本地化(如中文 GBK)shell 报错正文无 ASCII " error:",致 `exit 1` 逃熔断计数、
/// `last_error` 亦不回填。与 verify 侧 [`tool_output_failed`] 对齐,免判据分叉。
pub(crate) fn is_error_observation(obs: &str) -> bool {
    obs.contains(" error:")
        || obs.starts_with("BLOCKED")
        || obs.starts_with("permission denied")
        || (obs.starts_with("exit ") && !obs.starts_with("exit 0"))
}

pub(crate) fn durable_updates(call: &ToolCall, observation: &str) -> Vec<Patch> {
    if is_error_observation(observation) {
        let line = observation
            .lines()
            .next()
            .unwrap_or(observation)
            .to_string();
        return vec![Patch::SetLastError(Some(line))];
    }
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str());
    match call.name.as_str() {
        "write_file" | "edit_file" => arg("path")
            .map(|p| {
                vec![
                    Patch::RecordModified(p.to_string()),
                    Patch::SetLastError(None),
                ]
            })
            .unwrap_or_default(),
        "apply_edits" => {
            let mut ps: Vec<Patch> = parse_edits(call)
                .into_iter()
                .map(|e| Patch::RecordModified(e.path))
                .collect();
            if !ps.is_empty() {
                ps.push(Patch::SetLastError(None));
            }
            ps
        }
        _ => Vec::new(),
    }
}

/// 从 `todo_write` 的参数里抽出 `todos` 数组 → [`Todo`] 列表(status 缺省 `pending`)。
pub(crate) fn parse_todos(call: &ToolCall) -> Vec<Todo> {
    let Some(arr) = call.arguments.get("todos").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            let content = t.get("content").and_then(|v| v.as_str())?.to_string();
            let status = t
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .to_string();
            Some(Todo { content, status })
        })
        .collect()
}

/// 执行一个结构化工具调用,返回给模型看的观察结果(observation)。用真实的 `tools` crate 干活。
/// iter-40:前后各串一层 hook(pre_tool 可拦截 / post_tool fire-and-forget)。
pub fn execute_tool_call(call: &ToolCall) -> String {
    // pre_tool hook(iter-40):blocking hook 拒绝 → 不执行工具。
    if let Some(blocked) = run_pre_tool_hooks(call) {
        return blocked;
    }
    // 可观测(iter-44):核心动作埋点。`RUST_LOG=agent=debug` 可观 agent 每步工具调用。
    tracing::debug!(tool = %call.name, "tool call");
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let obs = match call.name.as_str() {
        "run_shell" => {
            let cmd = arg("cmd");
            // 危险命令拦截:即使用户批准也拒绝(无沙箱阶段的安全硬门槛)。
            if let Some(why) = tools::is_dangerous_command(cmd) {
                tracing::warn!(tool = %call.name, reason = %why, "blocked dangerous command");
                return format!("BLOCKED (dangerous: {why}) —— 拒绝执行 `{cmd}`");
            }
            // 约束守卫:删/清空受保护路径(测试)→ 拒(防奖励黑客)。
            if let Some(m) = constraint_guard_shell(cmd) {
                return m;
            }
            // 外置沙箱包裹(iter-46):配了 sandbox_cmd → 经它跑(真隔离交平台);否则宿主直跑。
            // 危险命令拦截/约束守卫已在上方先过 —— 沙箱是叠加的纵深防御,非替换。
            let result = match active_sandbox_cmd() {
                Some(sb) => {
                    let cwd = std::env::current_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    tracing::debug!(sandbox = %sb, "run_shell via sandbox");
                    tools::run_argv(&sandbox_argv(&sb, cmd, &cwd))
                }
                None => {
                    // 模型自主择 shell(iter-51):`shell` 字段选执行器,省则宿主默认(host_env 块已告知可用项)。
                    let shell = arg("shell");
                    tools::run_shell_in((!shell.is_empty()).then_some(shell), cmd)
                }
            };
            match result {
                Ok(r) => format!("exit {}: {}{}", r.code, r.stdout.trim(), r.stderr.trim()),
                Err(e) => format!("shell error: {e}"),
            }
        }
        "write_file" => {
            if let Err(e) = jail(arg("path")) {
                return e;
            }
            let contents = arg("contents");
            // 约束守卫:往受保护路径(测试)写空内容 = 清空测试 → 拒(防奖励黑客)。
            if let Some(m) = constraint_guard_write(arg("path"), contents) {
                return m;
            }
            match tools::write_file(arg("path"), contents) {
                Ok(()) => format!("wrote {} bytes to {}", contents.len(), arg("path")),
                Err(e) => format!("write error: {e}"),
            }
        }
        "edit_file" => {
            if let Err(e) = jail(arg("path")) {
                return e;
            }
            match tools::edit_file(arg("path"), arg("old_string"), arg("new_string")) {
                Ok(()) => format!("edited {}", arg("path")),
                Err(e) => format!("edit error: {e}"),
            }
        }
        "apply_edits" => {
            let edits = parse_edits(call);
            if edits.is_empty() {
                return "apply_edits error: 缺少 edits".to_string();
            }
            // 沙箱:任一路径越狱 → 整批拒(与 apply_edits 的原子性一致,不留半成品)。
            for e in &edits {
                if let Err(msg) = jail(&e.path) {
                    return msg;
                }
            }
            match tools::apply_edits(&edits) {
                Ok(n) => format!("applied {n} 个文件的批量编辑"),
                Err(e) => format!("apply_edits error: {e}"),
            }
        }
        "read_file" => {
            let num = |k: &str| call.arguments.get(k).and_then(|v| v.as_u64());
            let (off, lim) = (num("offset"), num("limit"));
            let res = if off.is_some() || lim.is_some() {
                tools::read_file_range(
                    arg("path"),
                    off.unwrap_or(1).max(1) as usize,
                    lim.unwrap_or(2000) as usize,
                )
            } else {
                tools::read_file(arg("path"))
            };
            match res {
                Ok(c) => c,
                Err(e) => format!("read error: {e}"),
            }
        }
        "search" => {
            let or = |k: &str, d: &'static str| {
                let v = arg(k);
                if v.is_empty() {
                    d.to_string()
                } else {
                    v.to_string()
                }
            };
            match tools::search(or("path", "."), arg("pattern"), &or("glob", "*")) {
                Ok(s) if s.is_empty() => "(no matches)".to_string(),
                Ok(s) => s,
                Err(e) => format!("search error: {e}"),
            }
        }
        // 状态更新在 act 节点(发 SetTodos patch);这里只回个观察摘要。
        "todo_write" => format!("已更新任务清单:{} 项", parse_todos(call).len()),
        "signal_write" => {
            let resolve = arg("resolve");
            if !resolve.is_empty() {
                return match signal_resolve(SIGNALS_DIR, resolve) {
                    Ok(true) => format!("signal resolved: {resolve}"),
                    Ok(false) => format!("signal 未找到: {resolve}"),
                    Err(e) => format!("signal error: {e}"),
                };
            }
            let body = arg("body");
            if body.is_empty() {
                return "signal error: 缺少 body".to_string();
            }
            let kind = if arg("type").is_empty() {
                "note"
            } else {
                arg("type")
            };
            match signal_create(SIGNALS_DIR, kind, body, "manual") {
                Ok(id) => format!("signal recorded: {id}"),
                Err(e) => format!("signal error: {e}"),
            }
        }
        // 未知/幻觉工具名:归一化为 **error**(含 " error:" → 喂失败信号 + 熔断计数),
        // 并提示只调系统所列工具。此前回 "unknown tool" 不含判据词 → 幻觉工具静默空转不计错。
        other => format!("tool error: 未知工具 `{other}`;请只调用系统所列工具"),
    };
    run_post_tool_hooks(call); // post_tool hook(iter-40):工具跑完 fire-and-forget(如写后格式化)。
    tracing::debug!(tool = %call.name, ok = !obs.contains(" error:"), "tool done");
    obs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::{tool_output_failed, tool_output_ok};
    use crate::context::durable_state_block;
    use crate::*;
    use langgraph::GraphState;
    use std::sync::Arc;

    /// 验证器抗奖励黑客:成功信号是**行首前缀** `exit 0:`,而非任意位置的 "exit 0" 子串。
    /// 失败命令(`exit 7:`)正文即便含 "exit 0" 文本,也不得被判成功;真实 `exit 0:` 成功仍认。
    #[test]
    fn tool_output_ok_requires_exit0_prefix_not_substring() {
        assert!(
            tool_output_ok("exit 0: build ok"),
            "真实 exit 0 前缀应算成功"
        );
        assert!(
            !tool_output_ok("exit 7: build failed, expected exit 0 but got 7"),
            "失败命令正文含 'exit 0' 文本不得被误判成功(堵奖励黑客/修正确性 bug)"
        );
        assert!(tool_output_ok("tests: passed"), "结构化 passed 标记仍认");
        assert!(!tool_output_ok("tests: 1 failed"), "failed 不算成功");
    }

    /// P0 物理闭环:shell 工具把真实退出码带回来(0 vs 非 0),不再是脚本假信号。
    #[test]
    fn shell_tool_reflects_real_exit_code() {
        let tool = shell_tool();
        assert!((tool.as_ref())("exit 0").starts_with("exit 0:"));
        assert!((tool.as_ref())("exit 7").starts_with("exit 7:"));
    }

    /// P1:结构化 tool_call → 真实文件写入(物理副作用可验证)。
    #[test]
    fn execute_tool_call_writes_real_file() {
        // 沙箱后:写路径须在 cwd 子树内,故用 cwd 相对唯一名(非 temp_dir)。
        let path = std::env::current_dir()
            .unwrap()
            .join("ridge_llm_toolcall.tmp");
        let _ = std::fs::remove_file(&path);
        let call = ToolCall {
            id: "x".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": path.to_str().unwrap(), "contents": "physical closure"}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.contains("wrote"), "{obs}");
        assert_eq!(tools::read_file(&path).unwrap(), "physical closure");
        let _ = std::fs::remove_file(&path);
    }

    /// iter-44:核心动作可观测 —— 危险命令拦截被 tracing 观测到(确定性、无文件副作用)。
    /// 线程本地 subscriber 捕获:execute_tool_call 同步、在测试线程跑,故可捕。
    #[test]
    fn execute_tool_call_traces_blocked_dangerous_command() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct BufWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for BufWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
            type Writer = BufWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(BufWriter(buf.clone()))
            .finish();

        let out = tracing::subscriber::with_default(sub, || {
            let call = ToolCall {
                id: "d".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "rm -rf /"}),
            };
            execute_tool_call(&call)
        });
        assert!(out.starts_with("BLOCKED"), "{out}"); // 无文件副作用
        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("run_shell"), "logged: {logged}");
        let low = logged.to_lowercase();
        assert!(
            low.contains("blocked") || low.contains("dangerous"),
            "logged: {logged}"
        );
    }

    /// 驾驭工程:结构化 edit_file tool_call → 精准替换真实文件(而非整文件覆写)。
    #[test]
    fn execute_tool_call_edits_real_file() {
        let path = std::env::current_dir().unwrap().join("ridge_llm_edit.tmp");
        tools::write_file(&path, "let n = 1;\n").unwrap();
        let call = ToolCall {
            id: "e".to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "let n = 1;",
                "new_string": "let n = 99;"
            }),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.starts_with("edited"), "{obs}");
        assert_eq!(tools::read_file(&path).unwrap(), "let n = 99;\n");
        let _ = std::fs::remove_file(&path);
    }

    /// 多文件批量编辑:一个 apply_edits 调用改 2 个文件,原子生效;preview 是一份汇总 diff。
    #[test]
    fn apply_edits_batches_multiple_files() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("ridge_agent_batch_tmp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        tools::write_file(&a, "one\n").unwrap();
        tools::write_file(&b, "two\n").unwrap();
        let call = ToolCall {
            id: "b".to_string(),
            name: "apply_edits".to_string(),
            arguments: serde_json::json!({"edits": [
                {"path": a.to_str().unwrap(), "old_string": "one", "new_string": "1"},
                {"path": b.to_str().unwrap(), "old_string": "two", "new_string": "2"},
            ]}),
        };
        // preview:一份汇总 diff,一次确认。
        let p = preview_call(&call);
        assert!(
            p.contains("批量编辑 2 处") && p.contains("- one") && p.contains("+ 2"),
            "{p}"
        );
        // 执行:两文件都改。
        let obs = execute_tool_call(&call);
        assert!(obs.contains("applied 2"), "{obs}");
        assert_eq!(tools::read_file(&a).unwrap(), "1\n");
        assert_eq!(tools::read_file(&b).unwrap(), "2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 用户交互:权限门看到的是**diff 预览**而非生 JSON —— 用户看着改动批准。
    #[test]
    fn preview_call_renders_edit_diff() {
        let call = ToolCall {
            id: "p".to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": "src/x.rs", "old_string": "old", "new_string": "new"
            }),
        };
        let p = preview_call(&call);
        assert!(p.contains("src/x.rs"), "{p}");
        assert!(p.contains("- old") && p.contains("+ new"), "diff 形态: {p}");
    }

    /// 静态底噪守护:工具 Schema 每轮都发,描述须精简且不回潮(去客套/内部机制/schema 重复)。
    #[test]
    fn tool_descriptions_stay_terse() {
        // 每工具 description 字符上限 —— 描述只说「做什么 + 何时用」,不复述 schema、不讲内部机制。
        const TOOL_DESC_MAX: usize = 120;
        let specs = builtin_tool_specs();
        assert!(!specs.is_empty());
        for s in &specs {
            let n = s.description.chars().count();
            assert!(
                n < TOOL_DESC_MAX,
                "工具 {} 描述 {n} 字,超上限 {TOOL_DESC_MAX} —— 精简它",
                s.name
            );
        }
    }

    /// 工具调用鲁棒:未知/幻觉工具名归一化为 error(喂失败信号 + 熔断计数),不再静默空转。
    #[test]
    fn unknown_tool_is_error_classified() {
        let call = ToolCall {
            id: "x".into(),
            name: "definitely_not_a_tool".into(),
            arguments: serde_json::json!({}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.contains("未知工具"), "应指出未知工具:{obs}");
        assert!(
            is_error_observation(&obs),
            "未知工具应被判为 error(喂熔断/失败信号)"
        );
        assert!(tool_output_failed(&obs), "未知工具应算失败信号");
    }

    /// 熔断漏判修复(iter-51):非零 `exit N` 必判错(即便正文无 ASCII " error:",如中文 GBK 报错),
    /// 与 verify 侧 tool_output_failed 对齐;`exit 0` 不误判。
    #[test]
    fn nonzero_exit_is_error_observation() {
        assert!(is_error_observation(
            "exit 1: 文件名、目录名或卷标语法不正确。"
        ));
        assert!(is_error_observation("exit 127: 'ls' 不是内部或外部命令"));
        assert!(!is_error_observation("exit 0: 一切正常"), "exit 0 不该误判");
        assert_eq!(
            is_error_observation("exit 1: 文件名、目录名或卷标语法不正确。"),
            tool_output_failed("exit 1: 文件名、目录名或卷标语法不正确。")
        );
    }

    /// Durable State 回填:写类工具成功 → 记 modified_files 清 last_error;工具错误 → 置 last_error。
    #[test]
    fn durable_state_backfill_from_tools() {
        let mut st = AgentState::new("t");
        let ok = ToolCall {
            id: "1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"src/a.rs","contents":"x"}),
        };
        for p in durable_updates(&ok, "wrote 1 bytes to src/a.rs") {
            st.apply(p);
        }
        assert!(st.modified_files.contains("src/a.rs"));
        assert!(st.last_error.is_none());

        let bad = ToolCall {
            id: "2".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({"path":"src/b.rs","old_string":"x","new_string":"y"}),
        };
        for p in durable_updates(&bad, "edit error: old_string 未找到") {
            st.apply(p);
        }
        assert_eq!(
            st.last_error.as_deref(),
            Some("edit error: old_string 未找到")
        );
        assert!(
            !st.modified_files.contains("src/b.rs"),
            "失败不记入已改文件"
        );
    }

    /// 事实驱动 O(1):反复改同两文件 50 步,事实块字符数恒定(不随步数膨胀)。
    #[test]
    fn durable_state_block_stays_bounded_over_steps() {
        let mut st = AgentState::new("t");
        let block_len = |st: &AgentState| {
            durable_state_block(st)
                .map(|b| b.chars().count())
                .unwrap_or(0)
        };
        let mut prev = 0;
        for i in 0..50 {
            let f = if i % 2 == 0 { "a.rs" } else { "b.rs" };
            let call = ToolCall {
                id: i.to_string(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": f, "contents":"x"}),
            };
            for p in durable_updates(&call, "wrote 1 bytes") {
                st.apply(p);
            }
            let now = block_len(&st);
            if i >= 2 {
                assert_eq!(now, prev, "事实块应有界恒定,step {i} 却变了");
            }
            prev = now;
        }
        assert_eq!(st.modified_files.len(), 2, "去重后仅 2 个文件");
    }

    /// 沙箱深度防御:越出 cwd 的绝对路径写 → `execute_tool_call` 硬拒(BLOCKED)且不落盘。
    #[test]
    fn jail_blocks_write_outside_cwd() {
        let outside = std::env::temp_dir().join("ridge_jail_evil_marker.txt");
        let _ = std::fs::remove_file(&outside);
        let call = ToolCall {
            id: "j".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": outside.to_str().unwrap(), "contents":"x"}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.starts_with("BLOCKED"), "越狱写应被拦: {obs}");
        assert!(!outside.exists(), "拦截后绝不落盘");
    }

    /// 安全硬门槛:危险命令即使走到 execute_tool_call 也被拦下,不执行。
    #[test]
    fn dangerous_shell_command_is_blocked() {
        let call = ToolCall {
            id: "x".to_string(),
            name: "run_shell".to_string(),
            arguments: serde_json::json!({"cmd": "rm -rf /"}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.starts_with("BLOCKED"), "{obs}");
    }

    /// P3 权限门:AutoDeny → 有副作用的工具不执行,观察为 permission denied,拿不到成功信号。
    #[tokio::test]
    async fn permission_gate_blocks_denied_tool() {
        use provider::{Completion, ScriptedProvider};
        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent_gated(Arc::new(scripted), McpTools::empty(), Arc::new(AutoDeny))
            .unwrap();
        let out = app.invoke(AgentState::new("build")).await.unwrap();

        assert!(out.messages.iter().any(|m| m.contains("permission denied")));
        assert!(!out.approved, "被拒的工具没真跑,拿不到 exit 0");
    }
}
