use super::*;

fn leaf(op: FfiExprOp, col: &str, v: i64) -> FfiExprNode {
    FfiExprNode { op, column: Some(col.into()), values: vec![FfiPropValue::Int(v)], children: vec![] }
}

#[test]
fn post_order_nodes_build_the_expected_tree() {
    let nodes = vec![
        leaf(FfiExprOp::Ge, "a", 1),
        leaf(FfiExprOp::Lt, "a", 5),
        FfiExprNode { op: FfiExprOp::And, column: None, values: vec![], children: vec![0, 1] },
        FfiExprNode { op: FfiExprOp::Not, column: None, values: vec![], children: vec![2] },
    ];
    let e = build_expr(nodes).unwrap().unwrap();
    assert_eq!(
        e,
        Expr::Not(Box::new(Expr::And(vec![
            Expr::Cmp("a".into(), CmpOp::Ge, 1.into()),
            Expr::Cmp("a".into(), CmpOp::Lt, 5.into()),
        ])))
    );
}

#[test]
fn malformed_trees_are_rejected() {
    let and_self = FfiExprNode { op: FfiExprOp::And, column: None, values: vec![], children: vec![0] };
    assert!(build_expr(vec![and_self]).is_err(), "forward/self reference");
    assert!(build_expr(vec![leaf(FfiExprOp::Eq, "a", 1), leaf(FfiExprOp::Eq, "b", 2)]).is_err(), "dangling node");
    let twice = FfiExprNode { op: FfiExprOp::Or, column: None, values: vec![], children: vec![0, 0] };
    assert!(build_expr(vec![leaf(FfiExprOp::Eq, "a", 1), twice]).is_err(), "reused node");
    let no_col = FfiExprNode { op: FfiExprOp::Eq, column: None, values: vec![FfiPropValue::Int(1)], children: vec![] };
    assert!(build_expr(vec![no_col]).is_err());
}
