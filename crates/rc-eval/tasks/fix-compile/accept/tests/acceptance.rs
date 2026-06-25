use fix_compile::double;

#[test]
fn acceptance_double() {
    assert_eq!(double(21), 42);
    assert_eq!(double(0), 0);
    assert_eq!(double(-3), -6);
}
