# NotebookLM 指导归档 + 对抗评审(iter-28,UI 三部曲收官刀)

> NLM 初稿:语义化 ANSI 色盘 + 行级 md 渲染 + 摘要折叠 + 启动帧序列 + 流式青游标。「不硬编 RGB、角色绑定 ANSI 16」与「帧序列纯函数验收」皆合本仓铁律,主体采纳。

## 裁决

### ✅ 采纳(P0):语义化色角色层
`Role` enum(Primary/Command/Info/Success/Error/Warn/Border/Muted/DiffAdd/DiffDel)→ `role_color` 映射 ANSI 16(ratatui 具名色,零 `Color::Rgb`);散落色值收口。尊重用户终端主题。

### ✅ 采纳(P0,改点):行级 markdown 轻渲染 —— **只在静态提交时染,流中不染**
`md_line_spans(line, in_code)`:``` 围栏切态(围栏行 Border 色)、块内 Muted、`#` 标题加粗 Primary、行内 `code`(Warn)与 `**bold**`(BOLD)扫描,未闭合记号按字面。不引解析库、不做表格/嵌套(NLM 边界同)。**改点**:NLM 提议 stream 尾巴也归 md 渲染 —— 驳,流中样式未定型(静态提交本旨:样式不再变才历史化),提交时染。

### ✅ 采纳(P0,简化):摘要折叠 —— 提交前纯函数折叠
`fold_lines(text, FOLD_MAX)`:超限留头 + `… (+N 行已折叠)` 尾标。在 flush 前应用,历史不刷屏;内核 `bound_observation` 已在源头有界,此为**呈现层**二次收敛。
- ❌ 驳 `/view <run_id>` 调阅命令(`.ridge/runs` 审计已可查,超本轮范围);❌ 驳 Live 区 Tab 折叠/展开切换(Tab 已归补全浮窗,模态冲突;复杂度不成比例)。

### ✅ 采纳(P1):启动帧序列 + 流式呼吸游标
- `splash_frame(tick, total)` 纯函数按列渐显 ASCII banner,借既有 100ms tick 驱动(10 tick ≈ 1s < 1.5s 上限),末帧整幅 `note` 入历史;非 TTY 天然不进 TUI。
- 流式尾巴 busy 时缀青色实心游标 `█`,按 `ui.frame` 奇偶 BOLD/DIM 交替 = 呼吸感,零额外状态。

### ❌ 驳回(幻觉/超范围)
- 「移除冗余 clear() 扫描」:本仓无此物,NLM 幻觉(引用 [8,13] 不对应本仓代码)。
- CJK wcwidth 宽度断言:自写 wcwidth 超范围;`wrapped_rows` 1 格近似已有 ponytail 注记,不动。

### 验收(纯函数)
`role_color` 映射断言(含无 Rgb 结构核);md:围栏切态/块内色/标题粗/行内 code span/未闭合字面;`fold_lines` 头保留 + `+N` 尾标 + 限内不动;`splash_frame` 首帧无字形、末帧全幅、单调渐显。
