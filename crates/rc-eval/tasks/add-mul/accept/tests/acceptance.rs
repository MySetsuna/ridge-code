use add_mul::{add, mul};

#[test]
fn acceptance_add() {
    assert_eq!(add(2, 3), 5);
    assert_eq!(add(-1, 1), 0);
}

#[test]
fn acceptance_mul() {
    assert_eq!(mul(2, 3), 6);
    assert_eq!(mul(0, 9), 0);
}
