//! 工具索引:把外部 MCP 工具归一化成内部 `ToolSpec` + 命名空间路由表。
//! 纯数据结构,与 rmcp 运行时解耦,可完全离线单测。

use rc_types::ToolSpec;
use serde_json::{json, Value};
use std::collections::HashMap;

/// 生成暴露给模型的命名空间工具名:`<server>__<tool>`。
/// 前缀确保多服务器同名工具、以及与内置工具之间不冲突。
pub(crate) fn namespaced_name(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
}

/// 暴露名 → (server_idx, 原始工具名) 的路由表 + 归一化后的 `ToolSpec` 列表。
#[derive(Default)]
pub(crate) struct ToolIndex {
    specs: Vec<ToolSpec>,
    route: HashMap<String, (usize, String)>,
}

impl ToolIndex {
    /// 登记一个工具:构造命名空间名 + `ToolSpec` + 路由项。
    pub(crate) fn add_tool(
        &mut self,
        server_idx: usize,
        server_name: &str,
        original: &str,
        description: &str,
        schema: Value,
    ) {
        let exposed = namespaced_name(server_name, original);
        if self.route.contains_key(&exposed) {
            tracing::warn!(tool = %exposed, "MCP 工具重名,后者覆盖前者(检查 [[mcp]] 的 name 是否唯一)");
        }
        self.specs.push(ToolSpec {
            name: exposed.clone(),
            description: description.to_string(),
            parameters: normalize_schema(schema),
        });
        self.route
            .insert(exposed, (server_idx, original.to_string()));
    }

    pub(crate) fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    pub(crate) fn has(&self, exposed_name: &str) -> bool {
        self.route.contains_key(exposed_name)
    }

    /// 暴露名 → (server_idx, 原始工具名)。
    pub(crate) fn route(&self, exposed_name: &str) -> Option<(usize, &str)> {
        self.route.get(exposed_name).map(|(i, n)| (*i, n.as_str()))
    }
}

/// 工具参数 schema 归一化:MCP 的 input_schema 本就是 JSON Schema object;
/// 若非 object 或缺 `type`,补成最小 object —— 部分 OpenAI 兼容端点要求 parameters 是带 type 的 object。
fn normalize_schema(schema: Value) -> Value {
    match schema {
        Value::Object(mut map) => {
            map.entry("type")
                .or_insert_with(|| Value::String("object".into()));
            Value::Object(map)
        }
        _ => json!({ "type": "object", "properties": {} }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_name_prefixes_server() {
        assert_eq!(namespaced_name("git", "status"), "git__status");
    }

    #[test]
    fn index_routes_across_servers_without_collision() {
        let mut idx = ToolIndex::default();
        let schema = json!({"type":"object","properties":{"q":{"type":"string"}}});
        idx.add_tool(0, "git", "search", "git 搜索", schema.clone());
        idx.add_tool(1, "fs", "search", "文件搜索", schema);

        // 两个 search 各自带前缀,不冲突。
        assert!(idx.has("git__search"));
        assert!(idx.has("fs__search"));
        assert_eq!(idx.specs().len(), 2);

        // 路由回正确的 (server_idx, 原名)。
        assert_eq!(idx.route("git__search"), Some((0, "search")));
        assert_eq!(idx.route("fs__search"), Some((1, "search")));
        assert_eq!(idx.route("nope"), None);

        // ToolSpec 暴露的是命名空间名。
        assert!(idx.specs().iter().any(|s| s.name == "git__search"));
    }

    #[test]
    fn normalize_schema_fills_type_and_handles_non_object() {
        // 非 object → 最小 object。
        assert_eq!(normalize_schema(json!("nope"))["type"], "object");
        // object 缺 type → 补上,且保留已有字段。
        let n = normalize_schema(json!({"properties":{"a":{}}}));
        assert_eq!(n["type"], "object");
        assert!(n["properties"]["a"].is_object());
    }
}
