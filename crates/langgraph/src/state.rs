use thiserror::Error;

/// 图运行期错误。库层用 thiserror(工程约定)。
#[derive(Debug, Error)]
pub enum GraphError {
    #[error("node `{0}` not found")]
    UnknownNode(String),

    #[error("no entry point set: add an edge from START to your first node")]
    NoEntry,

    #[error("edge from `{from}` points to unknown node `{to}`")]
    DanglingEdge { from: String, to: String },

    #[error("exceeded max supersteps ({0}); possible infinite loop")]
    StepLimit(usize),

    #[error("node `{node}` failed: {source}")]
    Node {
        node: String,
        #[source]
        source: BoxError,
    },

    #[error("node task panicked or was cancelled: {0}")]
    Join(String),

    #[error("best-of: no branch to select (empty input or all branches failed)")]
    NoWinner,
}

/// 节点错误统一归一化成这个 boxed 类型(同 provider 边界原则:对上归一化)。
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 智能体共享状态 —— 对应 LangGraph 的 State + channels/reducers。
///
/// 关键设计:节点**不直接修改**状态,而是返回一个 [`GraphState::Update`],
/// 由 [`GraphState::apply`](这就是 reducer)合并进来。这样同一个超步里多个节点
/// 并行产出的更新,能被确定性地归并——例如 `messages` 用**追加**而非覆盖,
/// 避免并发写同一字段导致的丢更新(见 `docs/REPORT-langgraph-rust.md` 的避坑指南)。
pub trait GraphState: Clone + Send + Sync + 'static {
    /// 节点产出的增量更新(delta)。
    type Update: Send + 'static;

    /// reducer:把一个更新合并进当前状态。
    ///
    /// LangGraph 不显式定义 reducer 时默认是「覆盖」,并发下很容易丢数据;
    /// 这里用 trait 强制每种状态自己声明合并语义。
    fn apply(&mut self, update: Self::Update);
}
