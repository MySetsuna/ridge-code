use std::sync::Mutex;

/// 一个超步结束时的状态快照。`step == 0` 是初始状态,`frontier` 是**下一个**超步要跑的节点。
#[derive(Clone, Debug)]
pub struct Checkpoint<S> {
    pub step: usize,
    pub frontier: Vec<String>,
    pub state: S,
}

/// checkpointer 抽象:执行环在每个超步后调用 [`save`](Checkpointer::save) 落一份快照。
///
/// 有了它就能做**故障恢复**和**时间旅行**(回滚到任意历史超步再分叉)。
pub trait Checkpointer<S>: Send + Sync {
    fn save(&self, checkpoint: Checkpoint<S>);
}

/// 内存版 checkpointer:append-only 存所有超步快照。
///
/// 用 `S: Clone` 做「秒级快照克隆」(Arc 内部数据直接 clone,零序列化开销)。
// ponytail: 内存 append-only,够做时间旅行 demo;要跨进程持久化就在 save 里加
// serde+bincode 落盘(见报告 §checkpoint),trait 不用改。
pub struct MemoryCheckpointer<S> {
    history: Mutex<Vec<Checkpoint<S>>>,
}

impl<S: Clone> MemoryCheckpointer<S> {
    pub fn new() -> Self {
        Self {
            history: Mutex::new(Vec::new()),
        }
    }

    /// 全部历史快照(用于审计 / 回放)。
    pub fn history(&self) -> Vec<Checkpoint<S>> {
        self.history.lock().unwrap().clone()
    }

    /// 取某个超步的快照 —— 时间旅行的入口:拿到后可作为新的 initial 再次 invoke。
    pub fn get(&self, step: usize) -> Option<Checkpoint<S>> {
        self.history
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.step == step)
            .cloned()
    }

    /// 最新一份快照。
    pub fn latest(&self) -> Option<Checkpoint<S>> {
        self.history.lock().unwrap().last().cloned()
    }
}

impl<S: Clone> Default for MemoryCheckpointer<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone + Send + Sync> Checkpointer<S> for MemoryCheckpointer<S> {
    fn save(&self, checkpoint: Checkpoint<S>) {
        self.history.lock().unwrap().push(checkpoint);
    }
}
