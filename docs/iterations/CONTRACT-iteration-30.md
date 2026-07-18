# CONTRACT · iteration-30 —— 输入 wcwidth 光标(正确性修复)

> maker = 用户需求原文第 4 条(「光标位置计算,没有落在真正的末端位置」)。checker = 本文正确性门禁。

## 缺口(代码索引核实)

`tui.rs` 全程用 `.chars().count()` 度量显示宽(`row_col` 的 col、真光标 x、`wrapped_rows` 折行、浮窗宽),`tui.rs:437` 有 ponytail 注自认「CJK 按 1 格计,偏差可容」。但终端里 CJK/emoji 实占 **2 单元格**,ratatui 的 `Paragraph`/边框按实占渲染 → **逻辑光标(char 序)正确,但屏幕落点按 1char=1 列算 → 偏左**,即用户所述「不落真末端」;折行低估行数 → 边框撕裂。`unicode-width` 未引。

## 目标

真光标落点、输入框折行高度、补全浮窗宽度全部改按 **wcwidth 显示单元格宽**。

## 设计

- 引 `unicode-width = "0.2"`(TUI 显示层标准纯数据宽度表,ratatui 本就间接依赖;非「内核精简铁律」所禁的外置计算能力)。
- `char_cells(c)` / `str_cells(s)` 显示宽辅助。
- `InputState::cursor_display_col() -> (row, display_col)`:CJK/emoji 按 `char_cells` 累加;真光标渲染改用它(替 `row_col`)。
- `wrapped_rows`、浮窗宽改 `str_cells`。
- **不改** 逻辑光标语义:`cursor` 仍是**字符序**(插删/Left/Right/Home/End 全不动),只在**渲染换算**处用显示宽 —— 最小面、零回归。

## 边界(不做)

- 软折行(单逻辑行超框宽)后的光标跟随 —— 既有近似不变,超范围。
- 输出块分隔/标头(用户需求第 5 条)—— 归后续视觉轮。

## 确定性验收信号

门禁全 exit 0。新增 `wcwidth_display_columns`:`char_cells('你')==2`、`str_cells("ab你好")==6`、`cursor_display_col` 于 `"你好a"`/多行/左移的显示列、`wrapped_rows("你你你",4)==2`(旧口径误判 1)。纯函数,零计时/PTY。

## 停机

单轮;连续 2 轮不过 → 报告。
