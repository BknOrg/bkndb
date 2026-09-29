//! Rebuilding filter expressions and queries from their FFI form.
use super::*;

/// A standalone filter (post-order nodes, as in `FfiQuery::filter`).
pub(crate) fn filter_expr(nodes: Vec<FfiExprNode>) -> Result<Option<Expr>, FfiBknError> {
    build_expr(nodes)
}

pub(super) fn build_expr(nodes: Vec<FfiExprNode>) -> Result<Option<Expr>, FfiBknError> {
    if nodes.is_empty() {
        return Ok(None);
    }
    let mut built: Vec<Option<Expr>> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.into_iter().enumerate() {
        let mut children = Vec::with_capacity(n.children.len());
        for &c in &n.children {
            let slot = built
                .get_mut(c as usize)
                .ok_or_else(|| invalid(format!("filter node {i} refers to child {c}, which is not an earlier node")))?;
            children.push(slot.take().ok_or_else(|| invalid(format!("filter node {c} is used more than once")))?);
        }
        let column = || n.column.clone().ok_or_else(|| invalid(format!("filter node {i} ({:?}) needs a column", n.op)));
        let one_value = || -> Result<_, FfiBknError> {
            match n.values.as_slice() {
                [v] => Ok(v.clone().into()),
                _ => Err(invalid(format!("filter node {i} ({:?}) needs exactly one value", n.op))),
            }
        };
        let cmp = |op| -> Result<Expr, FfiBknError> { Ok(Expr::Cmp(column()?, op, one_value()?)) };
        let expr = match n.op {
            FfiExprOp::Eq => cmp(CmpOp::Eq)?,
            FfiExprOp::Ne => cmp(CmpOp::Ne)?,
            FfiExprOp::Lt => cmp(CmpOp::Lt)?,
            FfiExprOp::Le => cmp(CmpOp::Le)?,
            FfiExprOp::Gt => cmp(CmpOp::Gt)?,
            FfiExprOp::Ge => cmp(CmpOp::Ge)?,
            FfiExprOp::In => Expr::In(column()?, n.values.iter().cloned().map(Into::into).collect()),
            FfiExprOp::IsNull => Expr::IsNull(column()?),
            FfiExprOp::IsNotNull => Expr::IsNotNull(column()?),
            FfiExprOp::StartsWith => match n.values.as_slice() {
                [FfiPropValue::Str(p)] => Expr::Prefix(column()?, p.clone()),
                _ => return Err(invalid(format!("filter node {i} (StartsWith) needs one string value"))),
            },
            FfiExprOp::Contains => Expr::Contains(column()?, one_value()?),
            FfiExprOp::Like | FfiExprOp::ILike => match n.values.as_slice() {
                [FfiPropValue::Str(p)] => Expr::Like(column()?, p.clone(), n.op == FfiExprOp::ILike),
                _ => return Err(invalid(format!("filter node {i} ({:?}) needs one string pattern", n.op))),
            },
            FfiExprOp::And => Expr::And(children),
            FfiExprOp::Or => Expr::Or(children),
            FfiExprOp::Not => match <[Expr; 1]>::try_from(children) {
                Ok([e]) => Expr::Not(Box::new(e)),
                Err(_) => return Err(invalid(format!("filter node {i} (Not) needs exactly one child"))),
            },
        };
        built.push(Some(expr));
    }
    let root = built.pop().flatten();
    if built.iter().any(Option::is_some) {
        return Err(invalid("filter has nodes that are not reachable from the root (the last node)"));
    }
    Ok(root)
}

impl TryFrom<FfiQuery> for Query {
    type Error = FfiBknError;
    fn try_from(q: FfiQuery) -> Result<Self, FfiBknError> {
        let mut query = Query::new();
        if let Some(e) = build_expr(q.filter)? {
            query = query.filter(e);
        }
        for o in q.order_by {
            query = query.order_by(&o.column, if o.descending { Order::Desc } else { Order::Asc });
        }
        query = query.offset(usize::try_from(q.offset).unwrap_or(usize::MAX));
        if let Some(l) = q.limit {
            query = query.limit(usize::try_from(l).unwrap_or(usize::MAX));
        }
        if let Some(cols) = q.columns {
            query = query.columns(cols);
        }
        Ok(query)
    }
}
