pub fn double(x: i64) -> i64 {
    // 类型错误:把 i64 赋给 String,无法编译。
    let y: String = x * 2;
    y
}
