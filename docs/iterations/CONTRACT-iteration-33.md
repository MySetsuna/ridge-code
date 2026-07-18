# CONTRACT · iteration-33 —— 允许输入排队(busy 时可提交,任务毕自动接跑)

> maker = 用户需求原文「允许输入排队」。checker = 本文正确性门禁。

## 缺口(代码索引核实)

`input_action` 里 `KeyCode::Enter if !busy => Submit` —— **busy 时 Enter 落 `Ignore`**,任务运行中无法提交下一条(测试 `input_action(Enter, busy=true)==Ignore` 固化此现状)。用户要:任务跑着时也能敲下一条,入队,当前任务毕自动接跑。

## 目标

busy 时提交 → **入队**(`ui.queued`),非 busy 时提交 → 正常起任务;任务 `done` 后若队非空,**自动取队首接跑**;忙碌粘条显待跑条数;Ctrl-C 中断连带清空队列(中止即取消全部待跑)。

## 设计(最小面 / 统一提交点)

- `Ui` 加 `queued: VecDeque<String>`。主环加 `pending_submit: Option<String>`。
- `InputAction` 加 `Queue`;`input_action`:`Enter if busy => Queue`、`Enter => Submit`(去掉 `!busy` 卫,`busy` 仍被 `Queue` 臂消费,无空参)。
- **统一提交点**在主环顶:`!ui.busy && pending_submit` 时消费之 —— `run_command`(斜杠即时)或起后台任务(原 Submit 内联逻辑上移至此**唯一**处,消除重复)。
- 键 `Submit` 臂 → 仅 `pending_submit = Some(input)`;新 `Queue` 臂 → `ui.queued.push_back(input)` + note 待跑条数。
- `done` 臂尾:`if pending_submit.is_none() { pending_submit = ui.queued.pop_front(); }` —— 下一圈顶点接跑。
- `Interrupt` 臂:`ui.queued.clear()`(中止清队)。
- 忙碌粘条:`fmt_busy_bar` 加 `queued` 参,>0 追加 ` · ⏳N`;`Vitals` 加 `queued` 字段,draw 传 `ui.queued.len()`。

## 边界(不做)

- 队列查看/编辑/重排 UI —— 超范围;仅入队 + 自动接跑 + 计数指示。
- 斜杠命令排队时的即时性 —— 统一按「入队,毕后顺序跑」,不为命令开快速通道(简单且可预期)。
- 持久化队列(重启续跑)—— 不做。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。测试改/增:
- `input_action(Enter, busy=true) == Queue`(改原 `Ignore` 断言);`input_action(Enter, busy=false) == Submit` 不变。
- `busy_bar_shows_queue_depth`:`fmt_busy_bar(...,queued=0)` 无 `⏳` 段;`queued=2` 含 ` · ⏳2`。

## 停机

单轮;连续 2 轮验收不过 → 报告。价值门禁不适用(用户明确需求)。
