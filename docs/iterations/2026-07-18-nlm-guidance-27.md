# NotebookLM 指导归档 + 对抗评审(iter-27,UI 三部曲第二刀)

> NLM 初稿:CSI u 握手 + 上下文感知历史召回(引 `207981a6` 的纯转换函数 —— 光标首行 Up 才召回,与本仓验收铁律天然同构)+ 补全浮窗。**本轮验收信号 NLM 首次全部合律**(转换断言/过滤纯函数/降级断言,零计时),记为正面样本。

## 裁决

### ✅ 采纳(P0):CSI u 握手 + 多行换行
- crossterm 0.28 `PushKeyboardEnhancementFlags` 真 API;**只推 `DISAMBIGUATE_ESCAPE_CODES`**(❌驳 `REPORT_EVENT_TYPES`:平添 press/release/repeat 事件噪声,我们本就滤 Release)。best-effort(旧终端/conhost 失败静默),Drop 时 `PopKeyboardEnhancementFlags`。
- **降级键修正**(NLM 提 Ctrl+J/Esc+Enter):Ctrl+J 在 unix legacy 恰是 LF 字节,与 Enter 不可区分 —— 不可靠。主降级改 **Alt+Enter**(终端发 ESC CR,crossterm 报 Enter+ALT,免协议全平台通);Ctrl+J 兼收(Windows 有效)。Windows 本就报 Shift+Enter(WinAPI 全键态),CSI u 主要惠及 unix 现代终端。

### ✅ 采纳(P0):InputState 输入状态机 + 历史召回
- 单 String append/pop 升级为 `InputState{buffer, cursor, history, hist_idx, draft}`:光标编辑(左右/行内插删/多行上下)、召回前存 draft、`Transition` 纯函数(首行 Up=召回,余=移光标)—— 全离线可测。光标行列按**逻辑行**('\n')计,折行内移动不做(ponytail)。
- 减法采纳:BPM 粘贴并入 `InputState.insert_str`(光标处原子插入);`input_action` Up/Down 空桩由新路由接管。

### ✅ 采纳(P1):`/` 命令 + `@` 路径补全浮窗
- 数据源:斜杠命令静态表;`@` 后缀词 → 该词目录 `read_dir`(单层,不递归,防 IO 卡 UI)。过滤 `starts_with`(❌驳「双层 LiteLLM 树」:名词乱入,无此需要)。键位:Tab 开/下一项,↑↓ 选,Enter 应用,Esc 关;**浮窗开时 ↑↓ 归浮窗**(模态优先级:审批 > 浮窗 > 输入)。
- ❌驳新文件 `completion.rs`:仓例 tui 单文件内聚,浮窗逻辑并入 tui.rs。

### 验收(采 NLM 案,细化)
路由矩阵纯函数(Shift/Alt+Enter→换行、首行 Up→召回、浮窗态 ↑↓/Enter/Esc 归浮窗、busy Enter 不提交);InputState 编辑操作(多行上下移列钳位、draft 存取);词提取 + 前缀过滤 + 应用替换纯函数。禁计时/PTY。
