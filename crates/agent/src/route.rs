//! Deterministic provider/model routing for agent roles.
//!
//! Route selection is policy, not model output: callers provide the task text,
//! role, and optional explicit overrides; this module filters and ranks only
//! candidates that the caller has already proved usable.

use std::fmt;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderRouteConfig {
    #[serde(default)]
    pub context_window: Option<usize>,
    #[serde(default)]
    pub cost_tier: Option<u8>,
    #[serde(default)]
    pub latency_tier: Option<u8>,
    #[serde(default)]
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub supports_reasoning: Option<bool>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDifficulty {
    Simple,
    #[default]
    Moderate,
    Complex,
}

impl TaskDifficulty {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "simple" | "easy" | "low" => Some(Self::Simple),
            "moderate" | "medium" | "normal" => Some(Self::Moderate),
            "complex" | "hard" | "high" => Some(Self::Complex),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Moderate => "moderate",
            Self::Complex => "complex",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl TaskSize {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "small" | "tiny" => Some(Self::Small),
            "medium" | "normal" => Some(Self::Medium),
            "large" | "big" => Some(Self::Large),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
    fn required_context_tokens(self) -> usize {
        match self {
            Self::Small => 4_096,
            Self::Medium => 16_384,
            Self::Large => 32_768,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    ReadOnly,
    Research,
    Planning,
    Coding,
    Review,
    #[default]
    General,
}

impl TaskKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "read" | "readonly" | "read_only" | "explore" => Some(Self::ReadOnly),
            "research" | "investigate" => Some(Self::Research),
            "plan" | "planning" | "design" => Some(Self::Planning),
            "code" | "coding" | "implement" | "implementation" => Some(Self::Coding),
            "review" | "audit" | "check" => Some(Self::Review),
            "general" | "other" => Some(Self::General),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Research => "research",
            Self::Planning => "planning",
            Self::Coding => "coding",
            Self::Review => "review",
            Self::General => "general",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRole {
    #[default]
    Subagent,
    Planner,
    Worker,
    Teammate,
    Checker,
    Maker,
}

impl RouteRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
            Self::Planner => "planner",
            Self::Worker => "worker",
            Self::Teammate => "teammate",
            Self::Checker => "checker",
            Self::Maker => "maker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskProfile {
    pub difficulty: TaskDifficulty,
    pub size: TaskSize,
    pub kind: TaskKind,
}

impl TaskProfile {
    pub fn infer(task: &str) -> Self {
        let lower = task.to_ascii_lowercase();
        let chars = task.chars().count();
        let size = if chars <= 400 {
            TaskSize::Small
        } else if chars <= 1_800 {
            TaskSize::Medium
        } else {
            TaskSize::Large
        };
        let kind = if contains_any(
            &lower,
            &[
                "read", "search", "explore", "inspect", "检索", "读取", "查看",
            ],
        ) {
            TaskKind::ReadOnly
        } else if contains_any(&lower, &["research", "investigate", "调研", "调查", "比较"]) {
            TaskKind::Research
        } else if contains_any(
            &lower,
            &["plan", "design", "architect", "规划", "设计", "架构"],
        ) {
            TaskKind::Planning
        } else if contains_any(
            &lower,
            &[
                "implement",
                "implementing",
                "fix",
                "edit",
                "write",
                "实现",
                "修复",
                "修改",
            ],
        ) {
            TaskKind::Coding
        } else if contains_any(&lower, &["review", "audit", "check", "审查", "审核"]) {
            TaskKind::Review
        } else {
            TaskKind::General
        };
        let complex_signal = contains_any(
            &lower,
            &[
                "complex",
                "architecture",
                "refactor",
                "root cause",
                "multi-step",
                "复杂",
                "重构",
                "根因",
            ],
        );
        let difficulty = if size == TaskSize::Large || complex_signal {
            TaskDifficulty::Complex
        } else if kind == TaskKind::ReadOnly && size == TaskSize::Small {
            TaskDifficulty::Simple
        } else {
            TaskDifficulty::Moderate
        };
        Self {
            difficulty,
            size,
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRequest {
    pub task: String,
    pub profile: TaskProfile,
    pub role: RouteRole,
    pub preferred_provider: Option<String>,
    pub preferred_model: Option<String>,
}

impl RouteRequest {
    pub fn from_task(task: impl Into<String>, role: RouteRole) -> Self {
        let task = task.into();
        Self {
            profile: TaskProfile::infer(&task),
            task,
            role,
            preferred_provider: None,
            preferred_model: None,
        }
    }

    /// Invalid optional overrides are ignored and the deterministic inference remains in force.
    pub fn with_overrides(
        mut self,
        difficulty: Option<&str>,
        size: Option<&str>,
        kind: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Self {
        if let Some(value) = difficulty.and_then(TaskDifficulty::parse) {
            self.profile.difficulty = value;
        }
        if let Some(value) = size.and_then(TaskSize::parse) {
            self.profile.size = value;
        }
        if let Some(value) = kind.and_then(TaskKind::parse) {
            self.profile.kind = value;
        }
        self.preferred_provider = non_empty(provider);
        self.preferred_model = non_empty(model);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelProfile {
    pub provider: String,
    pub model: String,
    pub kind: String,
    pub context_window: Option<usize>,
    pub cost_tier: Option<u8>,
    pub latency_tier: Option<u8>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub tags: Vec<String>,
}

impl ModelProfile {
    pub fn key(&self) -> String {
        format!("{}::{}", self.provider, self.model)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RouteRank {
    pub profile: ModelProfile,
    pub score: i32,
    pub eligible: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RouteDecision {
    pub selected: Option<ModelProfile>,
    pub ranking: Vec<RouteRank>,
    pub reason: String,
    pub used_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RouteAudit {
    pub role: RouteRole,
    pub selected: Option<String>,
    pub reason: String,
    pub used_fallback: bool,
}

impl RouteDecision {
    pub fn selected_key(&self) -> Option<String> {
        self.selected.as_ref().map(ModelProfile::key)
    }

    pub fn pinned(profile: ModelProfile, reason: impl Into<String>) -> Self {
        Self {
            selected: Some(profile.clone()),
            ranking: vec![RouteRank {
                profile,
                score: 0,
                eligible: true,
                reasons: vec!["explicit provider override".to_string()],
            }],
            reason: reason.into(),
            used_fallback: false,
        }
    }

    pub fn audit(&self, role: RouteRole) -> RouteAudit {
        RouteAudit {
            role,
            selected: self.selected_key(),
            reason: self.reason.clone(),
            used_fallback: self.used_fallback,
        }
    }
}

impl fmt::Display for RouteDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason)
    }
}

pub fn choose_route(request: &RouteRequest, candidates: &[ModelProfile]) -> RouteDecision {
    let mut ranking: Vec<RouteRank> = candidates
        .iter()
        .cloned()
        .map(|profile| rank_candidate(request, profile))
        .collect();
    ranking.sort_by(|left, right| {
        right
            .eligible
            .cmp(&left.eligible)
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.profile.key().cmp(&right.profile.key()))
    });
    let selected = ranking
        .iter()
        .find(|item| item.eligible)
        .map(|item| item.profile.clone());
    let reason = route_reason(request, &ranking, selected.as_ref());
    RouteDecision {
        selected,
        ranking,
        reason,
        used_fallback: false,
    }
}

fn route_reason(
    request: &RouteRequest,
    ranking: &[RouteRank],
    selected: Option<&ModelProfile>,
) -> String {
    let prefix = format!(
        "role={} difficulty={} size={} kind={}",
        request.role.as_str(),
        request.profile.difficulty.as_str(),
        request.profile.size.as_str(),
        request.profile.kind.as_str()
    );
    match selected {
        Some(profile) => {
            let score = ranking
                .iter()
                .find(|item| item.profile == *profile)
                .map(|item| item.score)
                .unwrap_or_default();
            format!(
                "{prefix} selected={} score={} candidates={}",
                profile.key(),
                score,
                ranking.len()
            )
        }
        None => format!("{prefix} selected=none reason=no eligible provider/model candidate"),
    }
}

fn rank_candidate(request: &RouteRequest, profile: ModelProfile) -> RouteRank {
    let mut reasons = Vec::new();
    let needs_tools = !matches!(request.role, RouteRole::Planner);
    let eligible = eligibility_reasons(request, &profile, needs_tools, &mut reasons);
    let score = score_candidate(request, &profile, needs_tools);
    RouteRank {
        profile,
        score,
        eligible,
        reasons,
    }
}

fn eligibility_reasons(
    request: &RouteRequest,
    profile: &ModelProfile,
    needs_tools: bool,
    reasons: &mut Vec<String>,
) -> bool {
    let mut eligible = true;
    if request
        .preferred_provider
        .as_deref()
        .is_some_and(|provider| provider != profile.provider)
    {
        eligible = false;
        reasons.push("provider preference mismatch".to_string());
    }
    if request
        .preferred_model
        .as_deref()
        .is_some_and(|model| model != profile.model)
    {
        eligible = false;
        reasons.push("model preference mismatch".to_string());
    }
    if profile
        .context_window
        .is_some_and(|window| window < request.profile.size.required_context_tokens())
    {
        eligible = false;
        reasons.push("context window too small".to_string());
    }
    if needs_tools && profile.supports_tools == Some(false) {
        eligible = false;
        reasons.push("tools unsupported".to_string());
    }
    if reasoning_rejected(request, profile, reasons) {
        eligible = false;
    }
    eligible
}

fn reasoning_rejected(
    request: &RouteRequest,
    profile: &ModelProfile,
    reasons: &mut Vec<String>,
) -> bool {
    let prefers_reasoning = matches!(
        request.role,
        RouteRole::Maker | RouteRole::Planner | RouteRole::Teammate
    ) || matches!(request.profile.difficulty, TaskDifficulty::Complex)
        || matches!(
            request.profile.kind,
            TaskKind::Planning | TaskKind::Research
        );
    if !prefers_reasoning {
        return false;
    }
    match profile.supports_reasoning {
        Some(true) => false,
        Some(false)
            if matches!(request.profile.difficulty, TaskDifficulty::Complex)
                || matches!(request.role, RouteRole::Maker | RouteRole::Planner) =>
        {
            reasons.push("reasoning unsupported for role/task".to_string());
            true
        }
        Some(false) => false,
        None => {
            reasons.push("reasoning capability unknown".to_string());
            false
        }
    }
}

fn score_candidate(request: &RouteRequest, profile: &ModelProfile, needs_tools: bool) -> i32 {
    let prefers_fast = matches!(
        request.role,
        RouteRole::Subagent | RouteRole::Checker | RouteRole::Worker
    ) || matches!(request.profile.difficulty, TaskDifficulty::Simple)
        || matches!(request.profile.kind, TaskKind::ReadOnly | TaskKind::Review);
    let mut score = tier_score(profile.latency_tier, true) * i32::from(prefers_fast);
    score += tier_score(profile.cost_tier, prefers_fast);
    score += reasoning_score(request, profile);
    if needs_tools && profile.supports_tools == Some(true) {
        score += 3;
    }
    if profile
        .context_window
        .is_some_and(|window| window >= request.profile.size.required_context_tokens())
    {
        score += 2;
    }
    score
}

fn reasoning_score(request: &RouteRequest, profile: &ModelProfile) -> i32 {
    let prefers_reasoning = matches!(
        request.role,
        RouteRole::Maker | RouteRole::Planner | RouteRole::Teammate
    ) || matches!(request.profile.difficulty, TaskDifficulty::Complex)
        || matches!(
            request.profile.kind,
            TaskKind::Planning | TaskKind::Research
        );
    if !prefers_reasoning {
        return 0;
    }
    match profile.supports_reasoning {
        Some(true) => 8,
        Some(false) => -5,
        None => 0,
    }
}

fn tier_score(tier: Option<u8>, prefer_low: bool) -> i32 {
    match (tier, prefer_low) {
        (Some(1), true) | (Some(3), false) => 5,
        (Some(2), _) => 2,
        (Some(3), true) | (Some(1), false) => -2,
        _ => 0,
    }
}

fn contains_any(text: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| text.contains(term))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{
        choose_route, ModelProfile, RouteRequest, RouteRole, TaskDifficulty, TaskKind, TaskProfile,
        TaskSize,
    };

    fn model(provider: &str, model: &str, reasoning: bool, cost: u8, latency: u8) -> ModelProfile {
        ModelProfile {
            provider: provider.to_string(),
            model: model.to_string(),
            kind: "openai".to_string(),
            context_window: Some(64_000),
            cost_tier: Some(cost),
            latency_tier: Some(latency),
            supports_tools: Some(true),
            supports_reasoning: Some(reasoning),
            tags: Vec::new(),
        }
    }

    #[test]
    fn infers_task_kind_size_and_difficulty() {
        let read = TaskProfile::infer("read the README and summarize the entrypoint");
        assert_eq!(read.kind, TaskKind::ReadOnly);
        assert_eq!(read.size, TaskSize::Small);
        assert_eq!(read.difficulty, TaskDifficulty::Simple);

        let design = TaskProfile::infer("design a complex architecture for a multi-step refactor");
        assert_eq!(design.kind, TaskKind::Planning);
        assert_eq!(design.difficulty, TaskDifficulty::Complex);
    }

    #[test]
    fn simple_subagent_prefers_fast_cheap_model() {
        let request = RouteRequest::from_task("search the config", RouteRole::Subagent);
        let decision = choose_route(
            &request,
            &[
                model("slow", "premium", true, 3, 3),
                model("fast", "small", false, 1, 1),
            ],
        );
        assert_eq!(decision.selected_key().as_deref(), Some("fast::small"));
    }

    #[test]
    fn complex_planner_prefers_reasoning_model() {
        let request = RouteRequest::from_task("design a complex architecture", RouteRole::Planner);
        let decision = choose_route(
            &request,
            &[
                model("fast", "small", false, 1, 1),
                model("deep", "reasoning", true, 3, 3),
            ],
        );
        assert_eq!(decision.selected_key().as_deref(), Some("deep::reasoning"));
    }

    #[test]
    fn filters_context_and_tools() {
        let mut no_tools = model("bad", "tiny", true, 1, 1);
        no_tools.supports_tools = Some(false);
        no_tools.context_window = Some(1_000);
        let good = model("good", "tool", true, 2, 2);
        let request = RouteRequest::from_task("implement the feature", RouteRole::Worker);
        let decision = choose_route(&request, &[no_tools, good.clone()]);
        assert_eq!(decision.selected, Some(good));
        assert!(decision.ranking.iter().any(|rank| !rank.eligible));
    }

    #[test]
    fn explicit_preference_overrides_score() {
        let request = RouteRequest::from_task("read the file", RouteRole::Subagent).with_overrides(
            None,
            None,
            None,
            Some("chosen"),
            None,
        );
        let decision = choose_route(
            &request,
            &[
                model("fast", "small", false, 1, 1),
                model("chosen", "large", true, 3, 3),
            ],
        );
        assert_eq!(decision.selected_key().as_deref(), Some("chosen::large"));
    }
}
