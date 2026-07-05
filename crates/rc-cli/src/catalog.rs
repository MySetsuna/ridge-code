//! 内置供应商目录:常见供应商的 base_url / kind / key 环境变量 + 示例模型。
//! 让用户「列出供应商/模型」并一键 `init` 配置,不必手写 base_url。
//!
//! 注:各家 base_url / kind / key 环境变量是稳定事实;**示例模型 ID 会随各家更新漂移**,
//! 以官方控制台/文档为准——`init` 可填任意 model id,不限于示例。

use crate::ProviderKind;

/// 目录里的一个供应商条目。
pub struct CatalogEntry {
    /// 引用名(用于 `models`/`init`,也写进配置的 provider name)。
    pub name: &'static str,
    /// wire 协议。
    pub kind: ProviderKind,
    /// OpenAI 兼容端点或 Anthropic 端点。
    pub base_url: &'static str,
    /// 建议的 key 环境变量名。
    pub api_key_env: &'static str,
    /// 是否本地/免费(无需付费 key,可直接跑)。
    pub free: bool,
    /// 一句话说明。
    pub note: &'static str,
    /// 示例模型 ID(非权威,可能更新)。
    pub models: &'static [&'static str],
}

/// 内置目录。base_url/kind/key 环境变量为稳定事实;模型仅示例。
pub fn catalog() -> &'static [CatalogEntry] {
    &[
        CatalogEntry {
            name: "anthropic",
            kind: ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com/v1",
            api_key_env: "ANTHROPIC_API_KEY",
            free: false,
            note: "原生 Anthropic(Claude),工具调用强",
            models: &["claude-sonnet-4-5", "claude-opus-4-1", "claude-haiku-4-5"],
        },
        CatalogEntry {
            name: "deepseek",
            kind: ProviderKind::Openai,
            base_url: "https://api.deepseek.com/v1",
            api_key_env: "DEEPSEEK_API_KEY",
            free: false,
            note: "DeepSeek,便宜、工具调用可用(避开纯推理 *-reasoner 做执行)",
            models: &["deepseek-chat", "deepseek-reasoner"],
        },
        CatalogEntry {
            name: "zhipu",
            kind: ProviderKind::Openai,
            base_url: "https://open.bigmodel.cn/api/paas/v4",
            api_key_env: "ZHIPUAI_API_KEY",
            free: false,
            note: "智谱 GLM(glm-4-flash 有免费额度)",
            models: &["glm-4.5", "glm-4.5-air", "glm-4-flash"],
        },
        CatalogEntry {
            name: "qwen",
            kind: ProviderKind::Openai,
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            api_key_env: "DASHSCOPE_API_KEY",
            free: false,
            note: "阿里 Qwen(DashScope 兼容端点)",
            models: &["qwen-plus", "qwen-max", "qwen2.5-coder-32b-instruct"],
        },
        CatalogEntry {
            name: "moonshot",
            kind: ProviderKind::Openai,
            base_url: "https://api.moonshot.cn/v1",
            api_key_env: "MOONSHOT_API_KEY",
            free: false,
            note: "月之暗面 Kimi",
            models: &["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"],
        },
        CatalogEntry {
            name: "openai",
            kind: ProviderKind::Openai,
            base_url: "https://api.openai.com/v1",
            api_key_env: "OPENAI_API_KEY",
            free: false,
            note: "OpenAI 官方",
            models: &["gpt-4o", "gpt-4o-mini", "o3-mini"],
        },
        CatalogEntry {
            name: "openrouter",
            kind: ProviderKind::Openai,
            base_url: "https://openrouter.ai/api/v1",
            api_key_env: "OPENROUTER_API_KEY",
            free: false,
            note: "OpenRouter 聚合(模型名带前缀,如 anthropic/…)",
            models: &[
                "anthropic/claude-sonnet-4.5",
                "deepseek/deepseek-chat",
                "qwen/qwen-2.5-coder-32b-instruct",
            ],
        },
        CatalogEntry {
            name: "groq",
            kind: ProviderKind::Openai,
            base_url: "https://api.groq.com/openai/v1",
            api_key_env: "GROQ_API_KEY",
            free: false,
            note: "Groq(超快推理)",
            models: &["llama-3.3-70b-versatile", "qwen-2.5-coder-32b"],
        },
        CatalogEntry {
            name: "nvidia",
            kind: ProviderKind::Openai,
            base_url: "https://integrate.api.nvidia.com/v1",
            api_key_env: "NVIDIA_API_KEY",
            free: false,
            note: "NVIDIA NIM",
            models: &[
                "meta/llama-3.3-70b-instruct",
                "qwen/qwen2.5-coder-32b-instruct",
            ],
        },
        CatalogEntry {
            name: "ollama",
            kind: ProviderKind::Openai,
            base_url: "http://localhost:11434/v1",
            api_key_env: "OLLAMA_API_KEY",
            free: true,
            note: "本地 Ollama,免费无需 key(先 `ollama pull <model>`)",
            models: &["qwen2.5-coder", "llama3.1", "deepseek-coder-v2"],
        },
    ]
}

/// 按名查找(大小写不敏感)。
pub fn find(name: &str) -> Option<&'static CatalogEntry> {
    catalog().iter().find(|e| e.name.eq_ignore_ascii_case(name))
}
