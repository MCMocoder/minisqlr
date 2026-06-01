/// Cascades Optimizer
///
/// This module keeps the optimizer independent from storage details for now:
/// it accepts a logical relational algebra tree and returns a physical plan
/// with estimated cost and provided physical properties.
use std::fmt::{Display, Formatter};

use crate::sql::ast::{BinaryOp, Expr, JoinConstraint, JoinType, OrderByItem, TableName};
use crate::sql::relation::{ProjectionItem, RelationExpr};

pub type GroupId = usize;
pub type Cost = f64;

#[derive(Clone, Debug, PartialEq)]
pub struct RequiredProperties {
    pub ordering: Vec<OrderByItem>,
}

impl RequiredProperties {
    pub fn any() -> Self {
        Self {
            ordering: Vec::new(),
        }
    }

    pub fn ordered(ordering: Vec<OrderByItem>) -> Self {
        Self { ordering }
    }
}

impl Default for RequiredProperties {
    fn default() -> Self {
        Self::any()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhysicalProperties {
    pub ordering: Vec<OrderByItem>,
}

impl PhysicalProperties {
    pub fn any() -> Self {
        Self {
            ordering: Vec::new(),
        }
    }

    pub fn ordered(ordering: Vec<OrderByItem>) -> Self {
        Self { ordering }
    }

    pub fn satisfies(&self, required: &RequiredProperties) -> bool {
        required.ordering.is_empty() || self.ordering == required.ordering
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PhysicalPlan {
    pub node: PhysicalPlanNode,
    pub cost: Cost,
    pub rows: f64,
    pub properties: PhysicalProperties,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PhysicalPlanNode {
    OneRow,
    SeqScan {
        table: TableName,
        alias: Option<String>,
    },
    Filter {
        predicate: Expr,
        input: Box<PhysicalPlan>,
    },
    Project {
        items: Vec<ProjectionItem>,
        input: Box<PhysicalPlan>,
    },
    NestedLoopJoin {
        join_type: JoinType,
        constraint: JoinConstraint,
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
    },
    HashJoin {
        join_type: JoinType,
        constraint: JoinConstraint,
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
    },
    Sort {
        items: Vec<OrderByItem>,
        input: Box<PhysicalPlan>,
    },
    Limit {
        count: Expr,
        input: Box<PhysicalPlan>,
    },
}

#[derive(Clone, Debug)]
pub struct OptimizerConfig {
    pub default_table_rows: f64,
    pub seq_scan_startup_cost: Cost,
    pub row_read_cost: Cost,
    pub predicate_cost: Cost,
    pub projection_cost: Cost,
    pub nested_loop_tuple_cost: Cost,
    pub hash_join_tuple_cost: Cost,
    pub sort_cost_factor: Cost,
    pub limit_selectivity: f64,
    pub initial_upper_bound: Cost,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            default_table_rows: 1_000.0,
            seq_scan_startup_cost: 1.0,
            row_read_cost: 0.01,
            predicate_cost: 0.002,
            projection_cost: 0.001,
            nested_loop_tuple_cost: 0.00005,
            hash_join_tuple_cost: 0.02,
            sort_cost_factor: 0.004,
            limit_selectivity: 0.1,
            initial_upper_bound: Cost::INFINITY,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OptimizerStats {
    pub explored_expressions: usize,
    pub pruned_expressions: usize,
    pub applied_enforcers: usize,
    pub applied_rules: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptimizeError {
    pub message: String,
}

impl OptimizeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for OptimizeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "optimizer error: {}", self.message)
    }
}

impl std::error::Error for OptimizeError {}

#[derive(Clone, Debug, PartialEq)]
struct MemoExpr {
    op: LogicalOp,
    children: Vec<GroupId>,
}

#[derive(Clone, Debug, PartialEq)]
enum LogicalOp {
    OneRow,
    TableScan {
        table: TableName,
        alias: Option<String>,
    },
    Projection {
        items: Vec<ProjectionItem>,
    },
    Selection {
        predicate: Expr,
    },
    Join {
        join_type: JoinType,
        constraint: JoinConstraint,
    },
    Order {
        items: Vec<OrderByItem>,
    },
    Limit {
        count: Expr,
    },
}

#[derive(Clone, Debug, Default)]
struct Group {
    expressions: Vec<MemoExpr>,
    explored: bool,
}

#[derive(Clone, Debug, Default)]
struct Memo {
    groups: Vec<Group>,
}

impl Memo {
    fn add_group(&mut self, expr: MemoExpr) -> GroupId {
        let group_id = self.groups.len();
        self.groups.push(Group {
            expressions: vec![expr],
            explored: false,
        });
        group_id
    }

    fn add_expression(&mut self, group_id: GroupId, expr: MemoExpr) -> bool {
        let group = &mut self.groups[group_id];
        if group.expressions.contains(&expr) {
            return false;
        }
        group.expressions.push(expr);
        true
    }
}

pub struct CascadesOptimizer {
    memo: Memo,
    config: OptimizerConfig,
    stats: OptimizerStats,
    best: Vec<Vec<(RequiredProperties, Option<PhysicalPlan>)>>,
}

impl CascadesOptimizer {
    pub fn new(config: OptimizerConfig) -> Self {
        Self {
            memo: Memo::default(),
            config,
            stats: OptimizerStats::default(),
            best: Vec::new(),
        }
    }

    pub fn optimize_relation(relation: &RelationExpr) -> Result<PhysicalPlan, OptimizeError> {
        Self::new(OptimizerConfig::default()).optimize(relation)
    }

    pub fn optimize(&mut self, relation: &RelationExpr) -> Result<PhysicalPlan, OptimizeError> {
        self.optimize_with_required(
            relation,
            RequiredProperties::any(),
            self.config.initial_upper_bound,
        )
    }

    pub fn optimize_with_required(
        &mut self,
        relation: &RelationExpr,
        required: RequiredProperties,
        upper_bound: Cost,
    ) -> Result<PhysicalPlan, OptimizeError> {
        self.memo = Memo::default();
        self.best.clear();
        self.stats = OptimizerStats::default();

        let root = self.insert_relation(relation);
        self.explore_group(root);
        self.best = vec![Vec::new(); self.memo.groups.len()];

        self.optimize_group(root, &required, upper_bound)
            .ok_or_else(|| OptimizeError::new("no physical plan found within cost bound"))
    }

    pub fn stats(&self) -> &OptimizerStats {
        &self.stats
    }

    fn insert_relation(&mut self, relation: &RelationExpr) -> GroupId {
        match relation {
            RelationExpr::OneRow => self.memo.add_group(MemoExpr {
                op: LogicalOp::OneRow,
                children: Vec::new(),
            }),
            RelationExpr::TableScan { table, alias } => self.memo.add_group(MemoExpr {
                op: LogicalOp::TableScan {
                    table: table.clone(),
                    alias: alias.clone(),
                },
                children: Vec::new(),
            }),
            RelationExpr::Projection { input, items } => {
                let child = self.insert_relation(input);
                self.memo.add_group(MemoExpr {
                    op: LogicalOp::Projection {
                        items: items.clone(),
                    },
                    children: vec![child],
                })
            }
            RelationExpr::Selection { input, predicate } => {
                let child = self.insert_relation(input);
                self.memo.add_group(MemoExpr {
                    op: LogicalOp::Selection {
                        predicate: predicate.clone(),
                    },
                    children: vec![child],
                })
            }
            RelationExpr::Join {
                left,
                right,
                join_type,
                constraint,
            } => {
                let left = self.insert_relation(left);
                let right = self.insert_relation(right);
                self.memo.add_group(MemoExpr {
                    op: LogicalOp::Join {
                        join_type: join_type.clone(),
                        constraint: constraint.clone(),
                    },
                    children: vec![left, right],
                })
            }
            RelationExpr::Order { input, items } => {
                let child = self.insert_relation(input);
                self.memo.add_group(MemoExpr {
                    op: LogicalOp::Order {
                        items: items.clone(),
                    },
                    children: vec![child],
                })
            }
            RelationExpr::Limit { input, count } => {
                let child = self.insert_relation(input);
                self.memo.add_group(MemoExpr {
                    op: LogicalOp::Limit {
                        count: count.clone(),
                    },
                    children: vec![child],
                })
            }
        }
    }

    fn explore_group(&mut self, group_id: GroupId) {
        if self.memo.groups[group_id].explored {
            return;
        }

        let expressions = self.memo.groups[group_id].expressions.clone();
        for expr in &expressions {
            for child in &expr.children {
                self.explore_group(*child);
            }
        }

        for expr in expressions {
            self.apply_rules(group_id, &expr);
        }

        self.memo.groups[group_id].explored = true;
    }

    fn apply_rules(&mut self, group_id: GroupId, expr: &MemoExpr) {
        match &expr.op {
            LogicalOp::Join {
                join_type,
                constraint,
            } if matches!(join_type, JoinType::Inner | JoinType::Cross) => {
                let swapped = MemoExpr {
                    op: LogicalOp::Join {
                        join_type: join_type.clone(),
                        constraint: constraint.clone(),
                    },
                    children: vec![expr.children[1], expr.children[0]],
                };
                if self.memo.add_expression(group_id, swapped) {
                    self.stats.applied_rules += 1;
                }
            }
            LogicalOp::Selection { predicate } => {
                let child_group_id = expr.children[0];
                let child_exprs = self.memo.groups[child_group_id].expressions.clone();
                for child_expr in child_exprs {
                    if let LogicalOp::Selection {
                        predicate: child_predicate,
                    } = child_expr.op
                    {
                        let combined = Expr::Binary {
                            left: Box::new(predicate.clone()),
                            op: BinaryOp::And,
                            right: Box::new(child_predicate),
                        };
                        let collapsed = MemoExpr {
                            op: LogicalOp::Selection {
                                predicate: combined,
                            },
                            children: child_expr.children,
                        };
                        if self.memo.add_expression(group_id, collapsed) {
                            self.stats.applied_rules += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn optimize_group(
        &mut self,
        group_id: GroupId,
        required: &RequiredProperties,
        upper_bound: Cost,
    ) -> Option<PhysicalPlan> {
        if let Some((_, cached)) = self.best[group_id]
            .iter()
            .find(|(props, _)| props == required)
        {
            return cached
                .clone()
                .and_then(|plan| (plan.cost <= upper_bound).then_some(plan));
        }

        let mut best_plan = None;
        let mut best_cost = upper_bound;
        let expressions = self.memo.groups[group_id].expressions.clone();

        for expr in expressions {
            let lower_bound = self.lower_bound_expr(&expr);
            if lower_bound >= best_cost {
                self.stats.pruned_expressions += 1;
                continue;
            }

            self.stats.explored_expressions += 1;
            if let Some(plan) = self.implement_expr(&expr, required, best_cost) {
                if plan.cost < best_cost {
                    best_cost = plan.cost;
                    best_plan = Some(plan);
                }
            }
        }

        self.best[group_id].push((required.clone(), best_plan.clone()));
        best_plan
    }

    fn implement_expr(
        &mut self,
        expr: &MemoExpr,
        required: &RequiredProperties,
        upper_bound: Cost,
    ) -> Option<PhysicalPlan> {
        match &expr.op {
            LogicalOp::OneRow => {
                let plan = PhysicalPlan {
                    node: PhysicalPlanNode::OneRow,
                    cost: 0.1,
                    rows: 1.0,
                    properties: PhysicalProperties::any(),
                };
                self.enforce(plan, required, upper_bound)
            }
            LogicalOp::TableScan { table, alias } => {
                let rows = self.config.default_table_rows;
                let plan = PhysicalPlan {
                    node: PhysicalPlanNode::SeqScan {
                        table: table.clone(),
                        alias: alias.clone(),
                    },
                    cost: self.config.seq_scan_startup_cost + rows * self.config.row_read_cost,
                    rows,
                    properties: PhysicalProperties::any(),
                };
                self.enforce(plan, required, upper_bound)
            }
            LogicalOp::Selection { predicate } => {
                let child = self.optimize_group(expr.children[0], required, upper_bound)?;
                let rows = (child.rows * 0.3).max(1.0);
                let cost = child.cost + child.rows * self.config.predicate_cost;
                let plan = PhysicalPlan {
                    node: PhysicalPlanNode::Filter {
                        predicate: predicate.clone(),
                        input: Box::new(child.clone()),
                    },
                    cost,
                    rows,
                    properties: child.properties,
                };
                self.enforce(plan, required, upper_bound)
            }
            LogicalOp::Projection { items } => {
                let child = self.optimize_group(expr.children[0], required, upper_bound)?;
                let cost = child.cost + child.rows * self.config.projection_cost;
                let plan = PhysicalPlan {
                    node: PhysicalPlanNode::Project {
                        items: items.clone(),
                        input: Box::new(child.clone()),
                    },
                    cost,
                    rows: child.rows,
                    properties: child.properties,
                };
                self.enforce(plan, required, upper_bound)
            }
            LogicalOp::Join {
                join_type,
                constraint,
            } => self.implement_join(expr, join_type, constraint, required, upper_bound),
            LogicalOp::Order { items } => {
                let order_required = RequiredProperties::ordered(items.clone());
                let child = self.optimize_group(expr.children[0], &order_required, upper_bound)?;
                self.enforce(child, required, upper_bound)
            }
            LogicalOp::Limit { count } => {
                let child = self.optimize_group(expr.children[0], required, upper_bound)?;
                let rows = (child.rows * self.config.limit_selectivity).max(1.0);
                let plan = PhysicalPlan {
                    node: PhysicalPlanNode::Limit {
                        count: count.clone(),
                        input: Box::new(child.clone()),
                    },
                    cost: child.cost + rows * 0.0005,
                    rows,
                    properties: child.properties,
                };
                self.enforce(plan, required, upper_bound)
            }
        }
    }

    fn implement_join(
        &mut self,
        expr: &MemoExpr,
        join_type: &JoinType,
        constraint: &JoinConstraint,
        required: &RequiredProperties,
        upper_bound: Cost,
    ) -> Option<PhysicalPlan> {
        let mut best_plan = None;
        let mut best_cost = upper_bound;

        let nested_left_required = if matches!(join_type, JoinType::Inner | JoinType::Cross) {
            RequiredProperties::any()
        } else {
            required.clone()
        };
        if let Some(left) = self.optimize_group(expr.children[0], &nested_left_required, best_cost)
        {
            if let Some(right) =
                self.optimize_group(expr.children[1], &RequiredProperties::any(), best_cost)
            {
                let rows = estimate_join_rows(left.rows, right.rows, join_type, constraint);
                let cost = left.cost
                    + right.cost
                    + left.rows * right.rows * self.config.nested_loop_tuple_cost;
                let plan = PhysicalPlan {
                    node: PhysicalPlanNode::NestedLoopJoin {
                        join_type: join_type.clone(),
                        constraint: constraint.clone(),
                        left: Box::new(left.clone()),
                        right: Box::new(right),
                    },
                    cost,
                    rows,
                    properties: left.properties,
                };
                if let Some(plan) = self.enforce(plan, required, best_cost) {
                    best_cost = plan.cost;
                    best_plan = Some(plan);
                }
            }
        }

        if matches!(join_type, JoinType::Inner) && is_equality_constraint(constraint) {
            let hash_lower_bound = self.lower_bound_group(expr.children[0])
                + self.lower_bound_group(expr.children[1])
                + self.config.hash_join_tuple_cost;
            if hash_lower_bound >= best_cost {
                self.stats.pruned_expressions += 1;
                return best_plan;
            }

            let left =
                self.optimize_group(expr.children[0], &RequiredProperties::any(), best_cost)?;
            let right =
                self.optimize_group(expr.children[1], &RequiredProperties::any(), best_cost)?;
            let rows = estimate_join_rows(left.rows, right.rows, join_type, constraint);
            let cost = left.cost
                + right.cost
                + (left.rows + right.rows) * self.config.hash_join_tuple_cost;
            let plan = PhysicalPlan {
                node: PhysicalPlanNode::HashJoin {
                    join_type: join_type.clone(),
                    constraint: constraint.clone(),
                    left: Box::new(left),
                    right: Box::new(right),
                },
                cost,
                rows,
                properties: PhysicalProperties::any(),
            };
            if let Some(plan) = self.enforce(plan, required, best_cost) {
                best_plan = Some(plan);
            }
        }

        best_plan
    }

    fn enforce(
        &mut self,
        plan: PhysicalPlan,
        required: &RequiredProperties,
        upper_bound: Cost,
    ) -> Option<PhysicalPlan> {
        if plan.properties.satisfies(required) {
            return (plan.cost <= upper_bound).then_some(plan);
        }

        if required.ordering.is_empty() {
            return (plan.cost <= upper_bound).then_some(plan);
        }

        let sort_cost = estimate_sort_cost(plan.rows, self.config.sort_cost_factor);
        let cost = plan.cost + sort_cost;
        if cost > upper_bound {
            self.stats.pruned_expressions += 1;
            return None;
        }

        self.stats.applied_enforcers += 1;
        let rows = plan.rows;
        Some(PhysicalPlan {
            node: PhysicalPlanNode::Sort {
                items: required.ordering.clone(),
                input: Box::new(plan),
            },
            cost,
            rows,
            properties: PhysicalProperties::ordered(required.ordering.clone()),
        })
    }

    fn lower_bound_expr(&self, expr: &MemoExpr) -> Cost {
        let child_bound: Cost = expr
            .children
            .iter()
            .map(|child| self.lower_bound_group(*child))
            .sum();
        child_bound
            + match &expr.op {
                LogicalOp::OneRow => 0.1,
                LogicalOp::TableScan { .. } => self.config.seq_scan_startup_cost,
                LogicalOp::Projection { .. } => self.config.projection_cost,
                LogicalOp::Selection { .. } => self.config.predicate_cost,
                LogicalOp::Join { .. } => self.config.hash_join_tuple_cost,
                LogicalOp::Order { .. } => 0.0,
                LogicalOp::Limit { .. } => 0.0,
            }
    }

    fn lower_bound_group(&self, group_id: GroupId) -> Cost {
        self.memo.groups[group_id]
            .expressions
            .iter()
            .map(|expr| self.lower_bound_expr(expr))
            .fold(Cost::INFINITY, Cost::min)
    }
}

fn estimate_join_rows(
    left_rows: f64,
    right_rows: f64,
    join_type: &JoinType,
    constraint: &JoinConstraint,
) -> f64 {
    match join_type {
        JoinType::Cross => left_rows * right_rows,
        JoinType::Inner if is_equality_constraint(constraint) => left_rows.max(right_rows),
        JoinType::Inner => (left_rows * right_rows * 0.3).max(1.0),
        JoinType::Left | JoinType::Right => left_rows.max(right_rows),
        JoinType::Full => left_rows + right_rows,
    }
}

fn estimate_sort_cost(rows: f64, factor: Cost) -> Cost {
    if rows <= 1.0 {
        return 0.0;
    }
    rows * rows.log2() * factor
}

fn is_equality_constraint(constraint: &JoinConstraint) -> bool {
    match constraint {
        JoinConstraint::Using(columns) => !columns.is_empty(),
        JoinConstraint::On(Expr::Binary {
            op: BinaryOp::Eq, ..
        }) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::sql::ast::{Expr, JoinType, Literal, OrderDirection, Statement};
    use crate::sql::parser::parse_sql;
    use crate::sql::relation::RelationExpr;

    use super::*;

    #[test]
    fn optimizes_simple_select_to_project_filter_scan() {
        let relation = relation_from_select("select id from users where age > 18");
        let mut optimizer = CascadesOptimizer::new(OptimizerConfig::default());

        let plan = optimizer.optimize(&relation).expect("plan");

        assert!(matches!(plan.node, PhysicalPlanNode::Project { .. }));
        let PhysicalPlanNode::Project { input, .. } = plan.node else {
            panic!("expected project");
        };
        assert!(matches!(input.node, PhysicalPlanNode::Filter { .. }));
    }

    #[test]
    fn sort_enforcer_satisfies_required_ordering() {
        let relation = relation_from_select("select id from users");
        let order_select = select_stmt("select id from users order by id desc");
        let required = RequiredProperties::ordered(order_select.order_by.clone());
        let mut optimizer = CascadesOptimizer::new(OptimizerConfig::default());

        let plan = optimizer
            .optimize_with_required(&relation, required.clone(), Cost::INFINITY)
            .expect("plan");

        assert!(plan.properties.satisfies(&required));
        assert!(contains_sort(&plan));
        assert_eq!(optimizer.stats().applied_enforcers, 1);
    }

    #[test]
    fn inner_equality_join_uses_hash_join_when_cheaper() {
        let relation =
            relation_from_select("select * from users u join orders o on u.id = o.user_id");
        let mut optimizer = CascadesOptimizer::new(OptimizerConfig::default());

        let plan = optimizer.optimize(&relation).expect("plan");

        assert!(contains_hash_join(&plan));
    }

    #[test]
    fn cascades_rule_adds_join_commutation_alternative() {
        let relation =
            relation_from_select("select * from users u join orders o on u.id = o.user_id");
        let mut optimizer = CascadesOptimizer::new(OptimizerConfig::default());

        let _ = optimizer.optimize(&relation).expect("plan");

        assert!(optimizer.stats().applied_rules > 0);
    }

    #[test]
    fn pruning_can_reject_plan_under_tight_upper_bound() {
        let relation = relation_from_select("select * from users");
        let mut config = OptimizerConfig::default();
        config.initial_upper_bound = 0.01;
        let mut optimizer = CascadesOptimizer::new(config);

        let err = optimizer.optimize(&relation).expect_err("should prune");

        assert!(err.message.contains("no physical plan"));
        assert!(optimizer.stats().pruned_expressions > 0);
    }

    #[test]
    fn logical_order_uses_sort_enforcer() {
        let relation = relation_from_select("select id from users order by id desc limit 5");
        let mut optimizer = CascadesOptimizer::new(OptimizerConfig::default());

        let plan = optimizer.optimize(&relation).expect("plan");

        assert!(matches!(plan.node, PhysicalPlanNode::Limit { .. }));
        assert!(contains_sort(&plan));
        assert_eq!(plan.rows, 100.0);
    }

    fn relation_from_select(sql: &str) -> RelationExpr {
        let select = select_stmt(sql);
        RelationExpr::from_select(&select).expect("relation")
    }

    fn select_stmt(sql: &str) -> crate::sql::ast::SelectStmt {
        let ast = parse_sql(sql).expect("parse");
        match ast.statements.into_iter().next().expect("statement") {
            Statement::Select(select) => select,
            _ => panic!("expected select"),
        }
    }

    fn contains_sort(plan: &PhysicalPlan) -> bool {
        match &plan.node {
            PhysicalPlanNode::Sort { .. } => true,
            PhysicalPlanNode::Filter { input, .. }
            | PhysicalPlanNode::Project { input, .. }
            | PhysicalPlanNode::Limit { input, .. } => contains_sort(input),
            PhysicalPlanNode::NestedLoopJoin { left, right, .. }
            | PhysicalPlanNode::HashJoin { left, right, .. } => {
                contains_sort(left) || contains_sort(right)
            }
            _ => false,
        }
    }

    fn contains_hash_join(plan: &PhysicalPlan) -> bool {
        match &plan.node {
            PhysicalPlanNode::HashJoin { join_type, .. } => *join_type == JoinType::Inner,
            PhysicalPlanNode::Filter { input, .. }
            | PhysicalPlanNode::Project { input, .. }
            | PhysicalPlanNode::Sort { input, .. }
            | PhysicalPlanNode::Limit { input, .. } => contains_hash_join(input),
            PhysicalPlanNode::NestedLoopJoin { left, right, .. } => {
                contains_hash_join(left) || contains_hash_join(right)
            }
            _ => false,
        }
    }

    #[test]
    fn keeps_limit_count_expr() {
        let relation = relation_from_select("select id from users limit 7");
        let mut optimizer = CascadesOptimizer::new(OptimizerConfig::default());

        let plan = optimizer.optimize(&relation).expect("plan");

        let PhysicalPlanNode::Limit { count, .. } = plan.node else {
            panic!("expected limit");
        };
        assert_eq!(count, Expr::Literal(Literal::Integer(7)));
    }

    #[test]
    fn logical_order_direction_is_preserved() {
        let relation = relation_from_select("select id from users order by id desc");
        let mut optimizer = CascadesOptimizer::new(OptimizerConfig::default());

        let plan = optimizer.optimize(&relation).expect("plan");

        let sort = find_sort(&plan).expect("sort");
        assert_eq!(sort[0].direction, OrderDirection::Desc);
    }

    fn find_sort(plan: &PhysicalPlan) -> Option<&[OrderByItem]> {
        match &plan.node {
            PhysicalPlanNode::Sort { items, .. } => Some(items),
            PhysicalPlanNode::Filter { input, .. }
            | PhysicalPlanNode::Project { input, .. }
            | PhysicalPlanNode::Limit { input, .. } => find_sort(input),
            PhysicalPlanNode::NestedLoopJoin { left, right, .. }
            | PhysicalPlanNode::HashJoin { left, right, .. } => {
                find_sort(left).or_else(|| find_sort(right))
            }
            _ => None,
        }
    }
}
