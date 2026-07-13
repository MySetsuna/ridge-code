use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// 一个超步结束时的状态快照。`step == 0` 是初始状态,`frontier` 是**下一个**超步要跑的节点。
/// serde 派生是**条件**的:只有当 `S: Serialize/Deserialize` 时快照才可落盘([`FileCheckpointer`]);
/// 纯内存路径([`MemoryCheckpointer`])不要求 `S` 可序列化。
#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// 落盘 checkpointer:每个超步快照 append 成一行 JSON(JSON Lines,append-only 版本日志)。
///
/// **耐用执行(M3)**:进程崩了/被 kill,新进程用同一路径 `new` 一个,读回历史,
/// 拿某个 `Checkpoint` 交给 [`CompiledGraph::resume`](crate::CompiledGraph::resume) 从那个超步续跑。
// ponytail: JSON Lines 够用且可读;要更小/更快就换 bincode,trait 不用改。
pub struct FileCheckpointer {
    path: std::path::PathBuf,
}

impl FileCheckpointer {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 读回全部历史快照(新进程恢复的入口)。文件不存在视为空。
    pub fn history<S: for<'de> Deserialize<'de>>(&self) -> std::io::Result<Vec<Checkpoint<S>>> {
        let data = match std::fs::read_to_string(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in data.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line).map_err(std::io::Error::other)?);
        }
        Ok(out)
    }

    /// 某个超步的快照(时间旅行 / 恢复点)。
    pub fn get<S: for<'de> Deserialize<'de>>(
        &self,
        step: usize,
    ) -> std::io::Result<Option<Checkpoint<S>>> {
        Ok(self.history()?.into_iter().find(|c| c.step == step))
    }

    /// 最新一份快照。
    pub fn latest<S: for<'de> Deserialize<'de>>(&self) -> std::io::Result<Option<Checkpoint<S>>> {
        Ok(self.history()?.pop())
    }
}

impl<S: Serialize + Send + Sync> Checkpointer<S> for FileCheckpointer {
    fn save(&self, checkpoint: Checkpoint<S>) {
        use std::io::Write;
        // 失败不炸执行环(与内置工具同风格):落盘出错只是丢一份快照,主流程照跑。
        if let Ok(line) = serde_json::to_string(&checkpoint) {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}
