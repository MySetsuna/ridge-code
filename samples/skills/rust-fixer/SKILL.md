---
name: rust-fixer
description: 修 Rust 编译错误 / 让测试通过 / 小步重构时用。触发词:编译不过、cargo build 报错、测试挂了、修 clippy。
---

# Rust 修复技能

修 Rust 代码时的工作法(maker≠checker,信编译器不信自述):

1. **先看客观信号**:`run_shell "cargo build"` 或 `cargo test`,读真实报错,别猜。
2. **先探再改**:`search` 定位、`read_file`(可分段)看清上下文,再动手。
3. **精准编辑**:改动优先 `edit_file`(唯一匹配替换),多处/跨文件用 `apply_edits`(原子、一次确认),**别整文件覆写**。
4. **改完必复核**:再 `cargo build` / `cargo test`,退出码 0 / 测试 passed 才算完 —— 模型说"修好了"不算数。
5. **最小改动**:只改导致报错的那点,别顺手重写无关代码。clippy 干净(`-D warnings`)、fmt 干净。
