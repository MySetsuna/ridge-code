//! 从 models.dev 实时拉取某供应商的当前模型列表(`ridge-code models <p> --online`)。
//! models.dev/api.json 是社区维护的 provider→models 目录,含真实 model id / 工具调用支持 / 上下文 / 价格。
//! 用动态 `Value` 解析(schema 大、只取少数字段,容忍缺失)。

use anyhow::{Context, Result};
use serde_json::Value;

const API_URL: &str = "https://models.dev/api.json";

/// 一个模型的关键信息。
pub struct ModelInfo {
    pub id: String,
    /// 是否支持工具调用(不支持则跑不了 agent 工具循环)。
    pub tool_call: bool,
    /// 上下文窗口 token 数。
    pub context: Option<u64>,
    /// 价格(USD / 每百万 token)。
    pub cost_in: Option<f64>,
    pub cost_out: Option<f64>,
}

/// 拉取并解析某 models.dev provider id 的模型列表(工具调用优先、再按 id 排)。
pub async fn fetch_models(provider_id: &str) -> Result<Vec<ModelInfo>> {
    let body = reqwest::Client::new()
        .get(API_URL)
        .send()
        .await
        .with_context(|| format!("请求 {API_URL} 失败(需联网)"))?
        .error_for_status()
        .context("models.dev 返回错误状态")?
        .text()
        .await
        .context("读取 models.dev 响应失败")?;
    let root: Value = serde_json::from_str(&body).context("解析 models.dev JSON 失败")?;
    let provider = root
        .get(provider_id)
        .with_context(|| format!("models.dev 无此供应商 id: {provider_id}"))?;
    let models = provider
        .get("models")
        .and_then(Value::as_object)
        .with_context(|| format!("models.dev 的 {provider_id} 无 models 字段"))?;

    let mut out: Vec<ModelInfo> = models
        .values()
        .filter_map(parse_model)
        .filter(|m| !m.id.is_empty())
        .collect();
    // 工具调用可用的排前面,便于挑能跑 agent 的。
    out.sort_by(|a, b| b.tool_call.cmp(&a.tool_call).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

fn parse_model(m: &Value) -> Option<ModelInfo> {
    let id = m.get("id").and_then(Value::as_str)?.to_string();
    Some(ModelInfo {
        id,
        tool_call: m.get("tool_call").and_then(Value::as_bool).unwrap_or(false),
        context: m
            .get("limit")
            .and_then(|l| l.get("context"))
            .and_then(Value::as_u64),
        cost_in: m
            .get("cost")
            .and_then(|c| c.get("input"))
            .and_then(Value::as_f64),
        cost_out: m
            .get("cost")
            .and_then(|c| c.get("output"))
            .and_then(Value::as_f64),
    })
}
