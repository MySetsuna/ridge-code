//! M0 内置工具:read_file / write_file / list_dir / run_shell。
//! schema 暂为手写(M0 求稳);后续可换 schemars derive(见 HANDOFF §3.2、§5)。

use anyhow::{anyhow, Context, Result};
use rc_types::{ToolCall, ToolSpec};
use serde_json::{json, Value};

/// 返回所有内置工具的规格(供 provider 传给模型)。
pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read_file".into(),
            description: "读取一个文本文件的全部内容".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径(相对工作目录或绝对)" }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "write_file".into(),
            description: "把内容写入文件(覆盖),自动创建父目录".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "content": { "type": "string", "description": "要写入的完整内容" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "list_dir".into(),
            description: "列出目录下的条目(目录名带尾部 /)".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径,默认当前目录" }
                },
                "required": []
            }),
        },
        ToolSpec {
            name: "run_shell".into(),
            description: "执行一条 shell 命令,返回 stdout / stderr 与退出码".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的命令" }
                },
                "required": ["command"]
            }),
        },
    ]
}

/// 执行一次工具调用;任何错误都转成给模型看的文本(让它能自我纠正)。
pub async fn dispatch(call: &ToolCall) -> String {
    match run(call).await {
        Ok(out) => out,
        Err(e) => format!("ERROR: {e:#}"),
    }
}

async fn run(call: &ToolCall) -> Result<String> {
    let args: Value = serde_json::from_str(&call.arguments)
        .with_context(|| format!("解析工具 {} 的参数失败: {}", call.name, call.arguments))?;
    match call.name.as_str() {
        "read_file" => {
            let path = str_arg(&args, "path")?;
            tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("读取 {path} 失败"))
        }
        "write_file" => {
            let path = str_arg(&args, "path")?;
            let content = str_arg(&args, "content")?;
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
            }
            tokio::fs::write(&path, &content)
                .await
                .with_context(|| format!("写入 {path} 失败"))?;
            Ok(format!("已写入 {path}({} 字节)", content.len()))
        }
        "list_dir" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".").to_string();
            let mut rd = tokio::fs::read_dir(&path)
                .await
                .with_context(|| format!("读取目录 {path} 失败"))?;
            let mut names = Vec::new();
            while let Some(e) = rd.next_entry().await? {
                let is_dir = e.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                let name = e.file_name().to_string_lossy().into_owned();
                names.push(if is_dir { format!("{name}/") } else { name });
            }
            names.sort();
            Ok(names.join("\n"))
        }
        "run_shell" => {
            let command = str_arg(&args, "command")?;
            shell(&command).await
        }
        other => Err(anyhow!("未知工具: {other}")),
    }
}

fn str_arg(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("缺少字符串参数 `{key}`"))
}

async fn shell(command: &str) -> Result<String> {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    let out = cmd
        .output()
        .await
        .with_context(|| format!("执行命令失败: {command}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code().unwrap_or(-1);
    Ok(format!(
        "exit={code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    ))
}
