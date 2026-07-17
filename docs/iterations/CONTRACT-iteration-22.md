# CONTRACT —— Iteration 22:TUI 交互专轨(首刀:修「滚动即拒绝」根因)

- **开工时间戳**: 2026-07-17
- **依据**: 用户「启 TUI 交互专轨」。iter-21 对抗评审把 TUI 重构列押后专轨;本轮开轨,**先修唯一的正确性 bug**(非审美打磨),按 ponytail 取最小、可确定性测的切片。
- **里程碑**: NLM notes(`1e0eb443`/`e86f5681`/`ec06937a`)反复点名「一滚动就拒绝」为交互头号痛点。根因:审批态输入捕获与滚动共用,除 `y`/`Enter` 外**一切键落 `_ => 拒绝`**。本轮以**模态决策纯函数**修根因。

## 目标(End State)

审批弹窗弹出时,用户按 `↑↓/PgUp/PgDn` **滚动看 diff** 不再误触拒绝、不消审批请求;仅 `y`/`Enter` 批准、`n`/`Esc` 拒绝,其余键忽略(等用户明确表态)。弹窗与 `/help` 文案如实说明该契约。

## 任务与验收信号(离线可测、无计时抖动)

| 优先级 | 任务 | 确定性验收信号 |
|---|---|---|
| **P0** | **模态决策纯函数** `approval_action(KeyCode) -> ApprovalAction{Approve,Reject,Scroll(i16),Ignore}` + `apply_scroll(u16,i16)->u16`(饱和);事件循环审批分支据此:滚动/忽略**不消** `pending`,仅批准/拒绝消 | 单测:滚动键 → `Scroll(±)` 非 Reject;`y`/`Enter`→Approve;`n`/`Esc`→Reject;字符/退格→Ignore(**不再误拒**);`apply_scroll` 上下界饱和不 panic |
| **P0** | **文案对齐**:审批 modal 与 `/help` 由「任意其他键: 拒绝」改为「y/Enter 批准 · n/Esc 拒绝 · ↑↓ 滚动看详情」 | 编译通过;字符串含新契约(人工/grep 可证) |

## 押后(TUI 专轨后续切片,不在本轮)

- **视口/滚动条/自动吸附底部**(`ScrollState` 虚拟列表、Home/End 置顶底):渲染层,验收多含计时,后续独立切片 + 用户在场验收。
- **分栏布局条件渲染 / 多行输入 `tui-textarea` / `/`@ 补全悬浮菜单**:交互增强,量较大。
- **启动 ASCII 动画 + Loading tips**:审美,最后做。
- **异步事件视口彻底解耦**:现架构已「执行图后台 task + 前台绘制/键盘」(见 tui.rs 头注),token 流不卡界面;进一步节流/虚拟列表属性能打磨,按需再做。

## 边界

不破坏现有 66(agent lib)+ 6(bin)+ 全工作区测试 + clippy/fmt 净;决策逻辑抽为**纯函数**(`KeyCode → ApprovalAction`),与渲染/IO 解耦、可单测、无计时抖动;`SyncSender::send(&self)` 允许滚动时**不消**审批请求(peek 而非 take);滚动只动 `ui.scroll`(u16 饱和,不 panic);不改审批的安全语义(危险命令仍需显式 y 批准,默认不放行)。

## 交付状态

> ✅ **已交付(2026-07-17)**。`approval_action`/`apply_scroll` 纯函数 + 事件循环审批分支重写(滚动/忽略不消 pending)+ modal/`/help` 文案对齐。`cargo test --workspace` 全绿(bin 6→**8**,+2:滚动键不误拒、饱和;agent lib 68 不变),clippy `-D warnings`、fmt 净。交互头号正确性 bug(滚动即拒绝)根除;审美/视口等打磨列后续切片。见 `docs/LOG.md` iter-22 条。
