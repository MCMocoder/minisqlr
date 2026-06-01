use std::fmt::{Display, Formatter};

use crate::sql::ast::{
    Expr, JoinConstraint, JoinType, OrderByItem, SelectItem, SelectItemKind, SelectStmt, TableName,
    TableRef,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationError {
    pub message: String,
}

impl RelationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for RelationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "relation error: {}", self.message)
    }
}

impl std::error::Error for RelationError {}

#[derive(Clone, Debug, PartialEq)]
pub enum RelationExpr {
    OneRow,
    TableScan {
        table: TableName,
        alias: Option<String>,
    },
    Projection {
        input: Box<RelationExpr>,
        items: Vec<ProjectionItem>,
    },
    Selection {
        input: Box<RelationExpr>,
        predicate: Expr,
    },
    Join {
        left: Box<RelationExpr>,
        right: Box<RelationExpr>,
        join_type: JoinType,
        constraint: JoinConstraint,
    },
    Order {
        input: Box<RelationExpr>,
        items: Vec<OrderByItem>,
    },
    Limit {
        input: Box<RelationExpr>,
        count: Expr,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProjectionItem {
    Expr { expr: Expr, alias: Option<String> },
    Wildcard,
}

impl RelationExpr {
    pub fn from_select(select: &SelectStmt) -> Result<Self, RelationError> {
        select_to_relation(select)
    }

    pub fn project(self, items: Vec<ProjectionItem>) -> Self {
        RelationExpr::Projection {
            input: Box::new(self),
            items,
        }
    }

    pub fn select(self, predicate: Expr) -> Self {
        RelationExpr::Selection {
            input: Box::new(self),
            predicate,
        }
    }

    pub fn order(self, items: Vec<OrderByItem>) -> Self {
        RelationExpr::Order {
            input: Box::new(self),
            items,
        }
    }

    pub fn limit(self, count: Expr) -> Self {
        RelationExpr::Limit {
            input: Box::new(self),
            count,
        }
    }
}

pub fn select_to_relation(select: &SelectStmt) -> Result<RelationExpr, RelationError> {
    let mut relation = match &select.from {
        Some(from) => table_ref_to_relation(from)?,
        None => RelationExpr::OneRow,
    };

    if let Some(predicate) = &select.where_clause {
        relation = relation.select(predicate.clone());
    }

    relation = relation.project(projection_items(&select.projection));

    if !select.order_by.is_empty() {
        relation = relation.order(select.order_by.clone());
    }

    if let Some(limit) = &select.limit {
        relation = relation.limit(limit.clone());
    }

    Ok(relation)
}

pub fn table_ref_to_relation(table_ref: &TableRef) -> Result<RelationExpr, RelationError> {
    let mut relation = RelationExpr::TableScan {
        table: table_ref.name.clone(),
        alias: table_ref.alias.clone(),
    };

    for join in &table_ref.joins {
        if matches!(
            (&join.join_type, &join.constraint),
            (
                JoinType::Cross,
                JoinConstraint::On(_) | JoinConstraint::Using(_)
            )
        ) {
            return Err(RelationError::new(
                "CROSS JOIN cannot have ON or USING constraint",
            ));
        }

        let right = table_ref_to_relation(&join.table)?;
        relation = RelationExpr::Join {
            left: Box::new(relation),
            right: Box::new(right),
            join_type: join.join_type.clone(),
            constraint: join.constraint.clone(),
        };
    }

    Ok(relation)
}

pub fn projection_items(items: &[SelectItem]) -> Vec<ProjectionItem> {
    items
        .iter()
        .map(|item| match &item.kind {
            SelectItemKind::Expr(expr) => ProjectionItem::Expr {
                expr: expr.clone(),
                alias: item.alias.clone(),
            },
            SelectItemKind::Wildcard => ProjectionItem::Wildcard,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::sql::ast::{
        BinaryOp, ColumnRef, Expr, JoinConstraint, JoinType, Literal, OrderDirection,
        SelectItemKind, Statement,
    };
    use crate::sql::parser::parse_sql;

    use super::*;

    #[test]
    fn lowers_select_into_scan_selection_projection() {
        let select = parse_select("select id, name as username from users where age >= 18");
        let relation = RelationExpr::from_select(&select).expect("relation");

        assert_eq!(
            relation,
            RelationExpr::Projection {
                input: Box::new(RelationExpr::Selection {
                    input: Box::new(RelationExpr::TableScan {
                        table: TableName::new("users"),
                        alias: None,
                    }),
                    predicate: Expr::Binary {
                        left: Box::new(Expr::Column(ColumnRef::new("age"))),
                        op: BinaryOp::Ge,
                        right: Box::new(Expr::Literal(Literal::Integer(18))),
                    },
                }),
                items: vec![
                    ProjectionItem::Expr {
                        expr: Expr::Column(ColumnRef::new("id")),
                        alias: None,
                    },
                    ProjectionItem::Expr {
                        expr: Expr::Column(ColumnRef::new("name")),
                        alias: Some("username".into()),
                    },
                ],
            }
        );
    }

    #[test]
    fn lowers_select_without_from_to_one_row_projection() {
        let select = parse_select("select 1 + 2");
        let relation = RelationExpr::from_select(&select).expect("relation");

        assert!(matches!(
            relation,
            RelationExpr::Projection {
                input,
                items: _
            } if matches!(*input, RelationExpr::OneRow)
        ));
    }

    #[test]
    fn lowers_join_chain_left_associatively() {
        let select = parse_select(
            "select * from users u join orders o on u.id = o.user_id left join payments p on o.id = p.order_id",
        );
        let relation = RelationExpr::from_select(&select).expect("relation");

        let RelationExpr::Projection { input, items } = relation else {
            panic!("expected projection");
        };
        assert_eq!(items, vec![ProjectionItem::Wildcard]);

        let RelationExpr::Join {
            left,
            right,
            join_type,
            constraint,
        } = *input
        else {
            panic!("expected outer join");
        };
        assert_eq!(join_type, JoinType::Left);
        assert!(matches!(constraint, JoinConstraint::On(_)));
        assert!(matches!(
            *right,
            RelationExpr::TableScan {
                table: TableName { name, .. },
                alias: Some(_)
            } if name == "payments"
        ));

        assert!(matches!(
            *left,
            RelationExpr::Join {
                join_type: JoinType::Inner,
                ..
            }
        ));
    }

    #[test]
    fn order_and_limit_wrap_projection() {
        let select = parse_select("select id from users order by id desc limit 5");
        let relation = RelationExpr::from_select(&select).expect("relation");

        let RelationExpr::Limit { input, count } = relation else {
            panic!("expected limit");
        };
        assert_eq!(count, Expr::Literal(Literal::Integer(5)));

        let RelationExpr::Order { input, items } = *input else {
            panic!("expected order");
        };
        assert_eq!(items[0].direction, OrderDirection::Desc);
        assert!(matches!(*input, RelationExpr::Projection { .. }));
    }

    #[test]
    fn projection_items_preserve_wildcard_and_alias() {
        let select = parse_select("select *, lower(name) as lname from users");
        let items = projection_items(&select.projection);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0], ProjectionItem::Wildcard);
        assert!(matches!(
            &select.projection[1].kind,
            SelectItemKind::Expr(Expr::FunctionCall { name, .. }) if name == "lower"
        ));
        assert!(matches!(
            &items[1],
            ProjectionItem::Expr {
                alias: Some(alias),
                ..
            } if alias == "lname"
        ));
    }

    fn parse_select(sql: &str) -> crate::sql::ast::SelectStmt {
        let ast = parse_sql(sql).expect("parse");
        match ast.statements.into_iter().next().expect("statement") {
            Statement::Select(select) => select,
            _ => panic!("expected select"),
        }
    }
}
