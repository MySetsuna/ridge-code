use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::checkpoint::{Checkpoint, Checkpointer};
use crate::state::{BoxError, GraphError, GraphState};

/// 图的虚拟起点。从 START 连一条边到你的第一个节点,就是设置入口。
pub const START: &str = "__start__";
/// 图的虚拟终点。节点连到 END(或没有出边)即该分支结束。
pub const END: &str = "__end__";

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type NodeResult<S> = Result<<S as GraphState>::Update, BoxError>;
type NodeFn<S> = Arc<dyn Fn(S) -> BoxFuture<NodeResult<S>> + Send + Sync>;
/// 条件边:看当前状态,返回下一步要激活的节点集合(可 fan-out 到多个)。
type Router<S> = Arc<dyn Fn(&S) -> Vec<String> + Send + Sync>;

/// 执行环发给订阅者的实时事件(streaming)。
#[derive(Clone)]
pub enum StreamEvent<S> {
    /// 某个节点在本超步执行完毕。
    NodeFinished { superstep: usize, node: String },
    /// 一个超步的同步点:更新已合并,`state` 是合并后的快照,`active` 是下一超步的节点。
    Superstep {
        step: usize,
        active: Vec<String>,
        state: S,
    },
}

/// 运行参数。
pub struct RunConfig {
    /// 硬性超步上限 —— 防跑飞的护栏。超过即 [`GraphError::StepLimit`]。
    pub max_supersteps: usize,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_supersteps: 100,
        }
    }
}

/// StateGraph 构建器:注册节点、静态边、条件边,最后 [`compile`](StateGraph::compile)。
pub struct StateGraph<S: GraphState> {
    nodes: HashMap<String, NodeFn<S>>,
    edges: HashMap<String, Vec<String>>,
    branches: HashMap<String, Router<S>>,
}

impl<S: GraphState> Default for StateGraph<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: GraphState> StateGraph<S> {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashMap::new(),
            branches: HashMap::new(),
        }
    }

    /// 注册一个节点:接收当前状态(快照),异步返回一个 `Update`(delta)。
    pub fn add_node<F, Fut, E>(&mut self, name: &str, f: F) -> &mut Self
    where
        F: Fn(S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<S::Update, E>> + Send + 'static,
        E: Into<BoxError> + 'static,
    {
        let wrapped: NodeFn<S> = Arc::new(move |state| {
            let fut = f(state);
            Box::pin(async move { fut.await.map_err(Into::into) })
        });
        self.nodes.insert(name.to_string(), wrapped);
        self
    }

    /// 静态边:`from` 跑完无条件激活 `to`。多次调用同一 `from` 即 fan-out。
    pub fn add_edge(&mut self, from: &str, to: &str) -> &mut Self {
        self.edges
            .entry(from.to_string())
            .or_default()
            .push(to.to_string());
        self
    }

    /// 设置入口 = 从 START 连一条边到 `to`。
    pub fn set_entry(&mut self, to: &str) -> &mut Self {
        self.add_edge(START, to)
    }

    /// 条件边:`from` 跑完后,由 `router` 看**合并后的状态**决定下一步去哪(可多个)。
    /// 条件边优先于静态边。
    pub fn add_conditional_edge<F>(&mut self, from: &str, router: F) -> &mut Self
    where
        F: Fn(&S) -> Vec<String> + Send + Sync + 'static,
    {
        self.branches.insert(from.to_string(), Arc::new(router));
        self
    }

    /// 校验并冻结成可执行图:入口存在、静态边不悬空。
    pub fn compile(self) -> Result<CompiledGraph<S>, GraphError> {
        let entry = self
            .edges
            .get(START)
            .and_then(|v| v.first())
            .ok_or(GraphError::NoEntry)?
            .clone();

        if entry != END && !self.nodes.contains_key(&entry) {
            return Err(GraphError::DanglingEdge {
                from: START.to_string(),
                to: entry,
            });
        }

        for (from, tos) in &self.edges {
            for to in tos {
                if to != END && !self.nodes.contains_key(to) {
                    return Err(GraphError::DanglingEdge {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
            }
        }

        Ok(CompiledGraph {
            nodes: self.nodes,
            edges: self.edges,
            branches: self.branches,
        })
    }
}

/// 编译后的图 + Pregel 超步执行环。
pub struct CompiledGraph<S: GraphState> {
    nodes: HashMap<String, NodeFn<S>>,
    edges: HashMap<String, Vec<String>>,
    branches: HashMap<String, Router<S>>,
}

impl<S: GraphState> CompiledGraph<S> {
    /// 跑到收敛(默认配置,无 checkpoint / 无 streaming)。
    pub async fn invoke(&self, initial: S) -> Result<S, GraphError> {
        self.invoke_with(initial, &RunConfig::default(), None, None)
            .await
    }

    /// 完整版:可挂 checkpointer(时间旅行)与事件通道(streaming)。
    ///
    /// 执行模型 = Pregel 超步 + BSP:一个超步内所有就绪节点看到**同一份**上一超步末的
    /// 状态快照(避免同级竞态),并发跑完后在**同步点**统一 reduce,再据合并后的状态路由。
    pub async fn invoke_with(
        &self,
        initial: S,
        cfg: &RunConfig,
        cp: Option<&dyn Checkpointer<S>>,
        tx: Option<&tokio::sync::mpsc::UnboundedSender<StreamEvent<S>>>,
    ) -> Result<S, GraphError> {
        let mut state = initial;
        let mut frontier: Vec<String> = self
            .edges
            .get(START)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n != END)
            .collect();

        if let Some(cp) = cp {
            cp.save(Checkpoint {
                step: 0,
                frontier: frontier.clone(),
                state: state.clone(),
            });
        }

        let mut step = 0usize;
        while !frontier.is_empty() {
            step += 1;
            if step > cfg.max_supersteps {
                return Err(GraphError::StepLimit(cfg.max_supersteps));
            }

            // BSP:所有节点拿到同一份快照,并发执行(tokio::spawn 跑满线程池)。
            let snapshot = state.clone();
            let mut handles = Vec::with_capacity(frontier.len());
            for node in &frontier {
                let f = self
                    .nodes
                    .get(node)
                    .ok_or_else(|| GraphError::UnknownNode(node.clone()))?
                    .clone();
                let s = snapshot.clone();
                let name = node.clone();
                handles.push(tokio::spawn(async move {
                    let r = f(s).await;
                    (name, r)
                }));
            }

            // 同步点:先收集结果,再按 frontier 顺序确定性地 reduce。
            let mut ran: Vec<String> = Vec::with_capacity(handles.len());
            for h in handles {
                let (node, res) = h.await.map_err(|e| GraphError::Join(e.to_string()))?;
                let update = res.map_err(|source| GraphError::Node {
                    node: node.clone(),
                    source,
                })?;
                state.apply(update);
                if let Some(tx) = tx {
                    let _ = tx.send(StreamEvent::NodeFinished {
                        superstep: step,
                        node: node.clone(),
                    });
                }
                ran.push(node);
            }

            // 据合并后的状态路由,算出下一超步的 frontier(去重 + 去 END)。
            let mut next: Vec<String> = Vec::new();
            for node in &ran {
                for succ in self.successors(node, &state) {
                    if succ != END && !next.contains(&succ) {
                        next.push(succ);
                    }
                }
            }

            if let Some(cp) = cp {
                cp.save(Checkpoint {
                    step,
                    frontier: next.clone(),
                    state: state.clone(),
                });
            }
            if let Some(tx) = tx {
                let _ = tx.send(StreamEvent::Superstep {
                    step,
                    active: next.clone(),
                    state: state.clone(),
                });
            }

            frontier = next;
        }

        Ok(state)
    }

    /// 一个节点跑完后的后继:条件边优先,否则静态边,都没有则隐式 END。
    fn successors(&self, node: &str, state: &S) -> Vec<String> {
        if let Some(router) = self.branches.get(node) {
            router(state)
        } else if let Some(tos) = self.edges.get(node) {
            tos.clone()
        } else {
            vec![END.to_string()]
        }
    }
}
