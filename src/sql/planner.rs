use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use crate::engine::btree::BTreeDataType;
use crate::sql::ast::{BinaryOp, ColumnRef, Expr, JoinConstraint, Literal, TableName, UnaryOp};
use crate::sql::operations::{
    Add, Close, Column, Divide, Eq, Ge, Goto, Gt, Halt, IfCursorEnd, IfNot, IfResultsGe, Integer,
    Le, Lt, MoveNext, Multiply, Ne, Negate, Not, Null, OpenRead, Real, ResultRow, Rewind, Subtract,
    Text, VMOperation,
};
use crate::sql::optimize::{PhysicalPlan, PhysicalPlanNode};
use crate::sql::relation::ProjectionItem;
use crate::sql::schema::{ColumnDef, SchemaManager};

#[derive(Clone, Debug, PartialEq)]
pub struct TableBinding {
    pub root_pgno: u32,
    pub columns: Vec<ColumnDef>,
}

pub trait Catalog {
    fn table(&self, name: &TableName) -> Option<TableBinding>;
}

impl Catalog for SchemaManager {
    fn table(&self, name: &TableName) -> Option<TableBinding> {
        let info = self.get_table(&name.name)?;
        let structure = SchemaManager::parse_structure(&info)?;
        Some(TableBinding {
            root_pgno: info.root_pgno,
            columns: structure.columns,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanError {
    pub message: String,
}

impl PlanError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for PlanError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "plan error: {}", self.message)
    }
}

impl std::error::Error for PlanError {}

pub fn compile_physical_plan<C: Catalog>(
    plan: &PhysicalPlan,
    catalog: &C,
) -> Result<Vec<Box<dyn VMOperation>>, PlanError> {
    PlanCompiler::new(catalog).compile(plan)
}

pub fn compile_physical_plan_body<C: Catalog>(
    plan: &PhysicalPlan,
    catalog: &C,
) -> Result<Vec<Box<dyn VMOperation>>, PlanError> {
    PlanCompiler::new(catalog).compile_body(plan)
}

struct PlanCompiler<'a, C: Catalog> {
    catalog: &'a C,
    builder: ProgramBuilder,
    next_cursor: usize,
    next_register: usize,
}

impl<'a, C: Catalog> PlanCompiler<'a, C> {
    fn new(catalog: &'a C) -> Self {
        Self {
            catalog,
            builder: ProgramBuilder::new(),
            next_cursor: 0,
            next_register: 0,
        }
    }

    fn compile(mut self, plan: &PhysicalPlan) -> Result<Vec<Box<dyn VMOperation>>, PlanError> {
        self.compile_program(plan, true)
    }

    fn compile_body(mut self, plan: &PhysicalPlan) -> Result<Vec<Box<dyn VMOperation>>, PlanError> {
        self.compile_program(plan, false)
    }

    fn compile_program(
        &mut self,
        plan: &PhysicalPlan,
        emit_halt: bool,
    ) -> Result<Vec<Box<dyn VMOperation>>, PlanError> {
        let mut output_limit = None;
        let output = self.peel_limit(plan, &mut output_limit)?;
        self.compile_output(output, output_limit)?;
        if emit_halt {
            self.builder.emit(Op::Halt);
        }
        std::mem::take(&mut self.builder).finish()
    }

    fn peel_limit<'p>(
        &self,
        plan: &'p PhysicalPlan,
        output_limit: &mut Option<usize>,
    ) -> Result<&'p PhysicalPlan, PlanError> {
        match &plan.node {
            PhysicalPlanNode::Limit { count, input } => {
                *output_limit = Some(literal_limit(count)?);
                self.peel_limit(input, output_limit)
            }
            PhysicalPlanNode::Sort { .. } => Err(PlanError::new(
                "Sort physical plan cannot be compiled before temp btree/sort VM ops exist",
            )),
            _ => Ok(plan),
        }
    }

    fn compile_output(
        &mut self,
        plan: &PhysicalPlan,
        output_limit: Option<usize>,
    ) -> Result<(), PlanError> {
        match &plan.node {
            PhysicalPlanNode::Project { items, input } => {
                let mut consumer = |compiler: &mut Self, env: &RowEnv| {
                    compiler.emit_projection(items, env, output_limit)
                };
                self.compile_rows(input, RowEnv::default(), &mut consumer)
            }
            _ => {
                let mut consumer = |compiler: &mut Self, env: &RowEnv| {
                    compiler.emit_wildcard_row(env, output_limit)
                };
                self.compile_rows(plan, RowEnv::default(), &mut consumer)
            }
        }
    }

    fn compile_rows(
        &mut self,
        plan: &PhysicalPlan,
        env: RowEnv,
        consumer: &mut dyn FnMut(&mut Self, &RowEnv) -> Result<(), PlanError>,
    ) -> Result<(), PlanError> {
        match &plan.node {
            PhysicalPlanNode::OneRow => consumer(self, &env),
            PhysicalPlanNode::SeqScan { table, alias } => {
                self.compile_seq_scan(table, alias, env, consumer)
            }
            PhysicalPlanNode::Filter { predicate, input } => {
                let mut inner = |compiler: &mut Self, env: &RowEnv| {
                    let cond = compiler.compile_expr(predicate, env)?;
                    let skip = compiler.builder.new_label();
                    compiler.builder.emit(Op::IfNot {
                        cond_reg: cond,
                        target: skip,
                    });
                    consumer(compiler, env)?;
                    compiler.builder.mark(skip);
                    Ok(())
                };
                self.compile_rows(input, env, &mut inner)
            }
            PhysicalPlanNode::Project { items, input } => {
                let mut inner = |compiler: &mut Self, env: &RowEnv| {
                    compiler.emit_projection(items, env, None)?;
                    consumer(compiler, env)
                };
                self.compile_rows(input, env, &mut inner)
            }
            PhysicalPlanNode::NestedLoopJoin {
                constraint,
                left,
                right,
                ..
            }
            | PhysicalPlanNode::HashJoin {
                constraint,
                left,
                right,
                ..
            } => self.compile_nested_loop_join(left, right, constraint, env, consumer),
            PhysicalPlanNode::Limit { count, input } => {
                let limit = literal_limit(count)?;
                let mut inner = |compiler: &mut Self, env: &RowEnv| {
                    compiler.emit_wildcard_row(env, Some(limit))
                };
                self.compile_rows(input, env, &mut inner)
            }
            PhysicalPlanNode::Sort { .. } => Err(PlanError::new(
                "Sort physical plan cannot be compiled before temp btree/sort VM ops exist",
            )),
        }
    }

    fn compile_seq_scan(
        &mut self,
        table: &TableName,
        alias: &Option<String>,
        env: RowEnv,
        consumer: &mut dyn FnMut(&mut Self, &RowEnv) -> Result<(), PlanError>,
    ) -> Result<(), PlanError> {
        let binding = self
            .catalog
            .table(table)
            .ok_or_else(|| PlanError::new(format!("unknown table '{}'", table.name)))?;
        let cursor_id = self.alloc_cursor();
        let mut scan_env = env;
        scan_env.sources.push(RowSource {
            table: table.name.clone(),
            alias: alias.clone(),
            cursor_id,
            columns: binding.columns,
        });

        let loop_start = self.builder.new_label();
        let loop_end = self.builder.new_label();
        self.builder.emit(Op::OpenRead {
            cursor_id,
            root_pgno: binding.root_pgno,
        });
        self.builder.emit(Op::Rewind { cursor_id });
        self.builder.emit(Op::IfCursorEnd { target: loop_end });
        self.builder.mark(loop_start);
        consumer(self, &scan_env)?;
        self.builder.emit(Op::MoveNext { cursor_id });
        self.builder.emit(Op::IfCursorEnd { target: loop_end });
        self.builder.emit(Op::Goto { target: loop_start });
        self.builder.mark(loop_end);
        self.builder.emit(Op::Close { cursor_id });
        Ok(())
    }

    fn compile_nested_loop_join(
        &mut self,
        left: &PhysicalPlan,
        right: &PhysicalPlan,
        constraint: &JoinConstraint,
        env: RowEnv,
        consumer: &mut dyn FnMut(&mut Self, &RowEnv) -> Result<(), PlanError>,
    ) -> Result<(), PlanError> {
        let mut left_consumer = |compiler: &mut Self, left_env: &RowEnv| {
            let left_source_len = left_env.sources.len();
            let mut right_consumer = |compiler: &mut Self, joined_env: &RowEnv| {
                if let Some(cond) =
                    compiler.compile_join_constraint(constraint, joined_env, left_source_len)?
                {
                    let skip = compiler.builder.new_label();
                    compiler.builder.emit(Op::IfNot {
                        cond_reg: cond,
                        target: skip,
                    });
                    consumer(compiler, joined_env)?;
                    compiler.builder.mark(skip);
                } else {
                    consumer(compiler, joined_env)?;
                }
                Ok(())
            };
            compiler.compile_rows(right, left_env.clone(), &mut right_consumer)
        };
        self.compile_rows(left, env, &mut left_consumer)
    }

    fn compile_join_constraint(
        &mut self,
        constraint: &JoinConstraint,
        env: &RowEnv,
        left_source_len: usize,
    ) -> Result<Option<usize>, PlanError> {
        match constraint {
            JoinConstraint::None => Ok(None),
            JoinConstraint::On(expr) => Ok(Some(self.compile_expr(expr, env)?)),
            JoinConstraint::Using(columns) => {
                let mut current = None;
                for column in columns {
                    let left =
                        self.compile_column_from_sources(column, &env.sources[..left_source_len])?;
                    let right =
                        self.compile_column_from_sources(column, &env.sources[left_source_len..])?;
                    let eq = self.alloc_register();
                    self.builder.emit(Op::Eq {
                        lhs: left,
                        rhs: right,
                        dest: eq,
                    });
                    current = match current {
                        Some(prev) => {
                            let both = self.alloc_register();
                            self.builder.emit(Op::And {
                                lhs: prev,
                                rhs: eq,
                                dest: both,
                            });
                            Some(both)
                        }
                        None => Some(eq),
                    };
                }
                Ok(current)
            }
        }
    }

    fn emit_projection(
        &mut self,
        items: &[ProjectionItem],
        env: &RowEnv,
        output_limit: Option<usize>,
    ) -> Result<(), PlanError> {
        let skip = self.emit_limit_guard(output_limit);
        let mut regs = Vec::new();
        for item in items {
            match item {
                ProjectionItem::Expr { expr, .. } => regs.push(self.compile_expr(expr, env)?),
                ProjectionItem::Wildcard => regs.extend(self.compile_all_columns(env)?),
            }
        }
        self.builder.emit(Op::ResultRow { fields: regs });
        if let Some(skip) = skip {
            self.builder.mark(skip);
        }
        Ok(())
    }

    fn emit_wildcard_row(
        &mut self,
        env: &RowEnv,
        output_limit: Option<usize>,
    ) -> Result<(), PlanError> {
        let skip = self.emit_limit_guard(output_limit);
        let fields = self.compile_all_columns(env)?;
        self.builder.emit(Op::ResultRow { fields });
        if let Some(skip) = skip {
            self.builder.mark(skip);
        }
        Ok(())
    }

    fn emit_limit_guard(&mut self, output_limit: Option<usize>) -> Option<Label> {
        output_limit.map(|limit| {
            let skip = self.builder.new_label();
            self.builder.emit(Op::IfResultsGe {
                limit,
                target: skip,
            });
            skip
        })
    }

    fn compile_all_columns(&mut self, env: &RowEnv) -> Result<Vec<usize>, PlanError> {
        let mut regs = Vec::new();
        for source in &env.sources {
            for col_idx in 0..source.columns.len() {
                let dest = self.alloc_register();
                self.builder.emit(Op::Column {
                    cursor_id: source.cursor_id,
                    col_idx,
                    dest,
                });
                regs.push(dest);
            }
        }
        Ok(regs)
    }

    fn compile_expr(&mut self, expr: &Expr, env: &RowEnv) -> Result<usize, PlanError> {
        match expr {
            Expr::Literal(literal) => self.compile_literal(literal),
            Expr::Column(column) => self.compile_column(column, env),
            Expr::Binary { left, op, right } => {
                let lhs = self.compile_expr(left, env)?;
                let rhs = self.compile_expr(right, env)?;
                let dest = self.alloc_register();
                match op {
                    BinaryOp::Eq => self.builder.emit(Op::Eq { lhs, rhs, dest }),
                    BinaryOp::Ne => self.builder.emit(Op::Ne { lhs, rhs, dest }),
                    BinaryOp::Lt => self.builder.emit(Op::Lt { lhs, rhs, dest }),
                    BinaryOp::Le => self.builder.emit(Op::Le { lhs, rhs, dest }),
                    BinaryOp::Gt => self.builder.emit(Op::Gt { lhs, rhs, dest }),
                    BinaryOp::Ge => self.builder.emit(Op::Ge { lhs, rhs, dest }),
                    BinaryOp::And => self.builder.emit(Op::And { lhs, rhs, dest }),
                    BinaryOp::Or => self.builder.emit(Op::Or { lhs, rhs, dest }),
                    BinaryOp::Add => self.builder.emit(Op::Add { lhs, rhs, dest }),
                    BinaryOp::Subtract => self.builder.emit(Op::Subtract { lhs, rhs, dest }),
                    BinaryOp::Multiply => self.builder.emit(Op::Multiply { lhs, rhs, dest }),
                    BinaryOp::Divide => self.builder.emit(Op::Divide { lhs, rhs, dest }),
                }
                Ok(dest)
            }
            Expr::Unary { op, expr } => {
                let src = self.compile_expr(expr, env)?;
                match op {
                    UnaryOp::Positive => Ok(src),
                    UnaryOp::Not => {
                        let dest = self.alloc_register();
                        self.builder.emit(Op::Not { src, dest });
                        Ok(dest)
                    }
                    UnaryOp::Negate => {
                        let dest = self.alloc_register();
                        self.builder.emit(Op::Negate { src, dest });
                        Ok(dest)
                    }
                }
            }
            Expr::FunctionCall { .. } => Err(PlanError::new(
                "function expressions are not supported by VM codegen yet",
            )),
        }
    }

    fn compile_literal(&mut self, literal: &Literal) -> Result<usize, PlanError> {
        let dest = self.alloc_register();
        match literal {
            Literal::Null => self.builder.emit(Op::Null { dest }),
            Literal::Integer(value) => self.builder.emit(Op::Integer {
                dest,
                value: *value,
            }),
            Literal::Real(value) => self.builder.emit(Op::Real {
                dest,
                value: *value,
            }),
            Literal::Text(value) => self.builder.emit(Op::Text {
                dest,
                value: value.as_bytes().to_vec(),
            }),
            Literal::Blob(value) => self.builder.emit(Op::Text {
                dest,
                value: value.clone(),
            }),
        }
        Ok(dest)
    }

    fn compile_column(&mut self, column: &ColumnRef, env: &RowEnv) -> Result<usize, PlanError> {
        let source = env.resolve(column)?;
        let dest = self.alloc_register();
        self.builder.emit(Op::Column {
            cursor_id: source.cursor_id,
            col_idx: source.col_idx,
            dest,
        });
        Ok(dest)
    }

    fn compile_column_from_sources(
        &mut self,
        column: &str,
        sources: &[RowSource],
    ) -> Result<usize, PlanError> {
        let mut matches = sources.iter().filter_map(|source| {
            source
                .columns
                .iter()
                .position(|col| col.name.eq_ignore_ascii_case(column))
                .map(|col_idx| ResolvedColumn {
                    cursor_id: source.cursor_id,
                    col_idx,
                })
        });
        let resolved = matches
            .next()
            .ok_or_else(|| PlanError::new(format!("unknown USING column '{column}'")))?;
        if matches.next().is_some() {
            return Err(PlanError::new(format!("ambiguous USING column '{column}'")));
        }
        let dest = self.alloc_register();
        self.builder.emit(Op::Column {
            cursor_id: resolved.cursor_id,
            col_idx: resolved.col_idx,
            dest,
        });
        Ok(dest)
    }

    fn alloc_cursor(&mut self) -> usize {
        let cursor = self.next_cursor;
        self.next_cursor += 1;
        cursor
    }

    fn alloc_register(&mut self) -> usize {
        let register = self.next_register;
        self.next_register += 1;
        register
    }
}

#[derive(Clone, Debug, Default)]
struct RowEnv {
    sources: Vec<RowSource>,
}

impl RowEnv {
    fn resolve(&self, column: &ColumnRef) -> Result<ResolvedColumn, PlanError> {
        let mut matches = self.sources.iter().filter_map(|source| {
            if let Some(table) = &column.table {
                if !source.matches_table(table) {
                    return None;
                }
            }
            source
                .columns
                .iter()
                .position(|col| col.name.eq_ignore_ascii_case(&column.name))
                .map(|col_idx| ResolvedColumn {
                    cursor_id: source.cursor_id,
                    col_idx,
                })
        });

        let resolved = matches
            .next()
            .ok_or_else(|| PlanError::new(format!("unknown column '{}'", column.name)))?;
        if matches.next().is_some() {
            return Err(PlanError::new(format!(
                "ambiguous column '{}'",
                column.name
            )));
        }
        Ok(resolved)
    }
}

#[derive(Clone, Debug)]
struct RowSource {
    table: String,
    alias: Option<String>,
    cursor_id: usize,
    columns: Vec<ColumnDef>,
}

impl RowSource {
    fn matches_table(&self, name: &str) -> bool {
        self.table.eq_ignore_ascii_case(name)
            || self
                .alias
                .as_ref()
                .map(|alias| alias.eq_ignore_ascii_case(name))
                .unwrap_or(false)
    }
}

#[derive(Clone, Debug)]
struct ResolvedColumn {
    cursor_id: usize,
    col_idx: usize,
}

type Label = usize;

#[derive(Clone, Debug)]
enum Op {
    OpenRead {
        cursor_id: usize,
        root_pgno: u32,
    },
    Close {
        cursor_id: usize,
    },
    Rewind {
        cursor_id: usize,
    },
    MoveNext {
        cursor_id: usize,
    },
    IfCursorEnd {
        target: Label,
    },
    IfResultsGe {
        limit: usize,
        target: Label,
    },
    Goto {
        target: Label,
    },
    IfNot {
        cond_reg: usize,
        target: Label,
    },
    Integer {
        dest: usize,
        value: i64,
    },
    Real {
        dest: usize,
        value: f64,
    },
    Text {
        dest: usize,
        value: Vec<u8>,
    },
    Null {
        dest: usize,
    },
    Column {
        cursor_id: usize,
        col_idx: usize,
        dest: usize,
    },
    Eq {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Ne {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Lt {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Le {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Gt {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Ge {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    And {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Or {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Not {
        src: usize,
        dest: usize,
    },
    Negate {
        src: usize,
        dest: usize,
    },
    Add {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Subtract {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Multiply {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    Divide {
        lhs: usize,
        rhs: usize,
        dest: usize,
    },
    ResultRow {
        fields: Vec<usize>,
    },
    Halt,
}

#[derive(Clone, Debug, Default)]
struct ProgramBuilder {
    ops: Vec<Op>,
    labels: HashMap<Label, usize>,
    next_label: Label,
}

impl ProgramBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn new_label(&mut self) -> Label {
        let label = self.next_label;
        self.next_label += 1;
        label
    }

    fn mark(&mut self, label: Label) {
        self.labels.insert(label, self.ops.len());
    }

    fn emit(&mut self, op: Op) {
        self.ops.push(op);
    }

    fn finish(self) -> Result<Vec<Box<dyn VMOperation>>, PlanError> {
        self.ops
            .into_iter()
            .map(|op| op.into_vm_operation(&self.labels))
            .collect()
    }
}

impl Op {
    fn into_vm_operation(
        self,
        labels: &HashMap<Label, usize>,
    ) -> Result<Box<dyn VMOperation>, PlanError> {
        Ok(match self {
            Op::OpenRead {
                cursor_id,
                root_pgno,
            } => Box::new(OpenRead {
                cursor_id,
                root_pgno,
            }),
            Op::Close { cursor_id } => Box::new(Close { cursor_id }),
            Op::Rewind { cursor_id } => Box::new(Rewind { cursor_id }),
            Op::MoveNext { cursor_id } => Box::new(MoveNext { cursor_id }),
            Op::IfCursorEnd { target } => Box::new(IfCursorEnd {
                target: resolve_label(labels, target)?,
            }),
            Op::IfResultsGe { limit, target } => Box::new(IfResultsGe {
                limit,
                target: resolve_label(labels, target)?,
            }),
            Op::Goto { target } => Box::new(Goto {
                target: resolve_label(labels, target)?,
            }),
            Op::IfNot { cond_reg, target } => Box::new(IfNot {
                cond_reg,
                target: resolve_label(labels, target)?,
            }),
            Op::Integer { dest, value } => Box::new(Integer { dest, value }),
            Op::Real { dest, value } => Box::new(Real { dest, value }),
            Op::Text { dest, value } => Box::new(Text { dest, value }),
            Op::Null { dest } => Box::new(Null { dest }),
            Op::Column {
                cursor_id,
                col_idx,
                dest,
            } => Box::new(Column {
                cursor_id,
                col_idx,
                dest,
            }),
            Op::Eq { lhs, rhs, dest } => Box::new(Eq { lhs, rhs, dest }),
            Op::Ne { lhs, rhs, dest } => Box::new(Ne { lhs, rhs, dest }),
            Op::Lt { lhs, rhs, dest } => Box::new(Lt { lhs, rhs, dest }),
            Op::Le { lhs, rhs, dest } => Box::new(Le { lhs, rhs, dest }),
            Op::Gt { lhs, rhs, dest } => Box::new(Gt { lhs, rhs, dest }),
            Op::Ge { lhs, rhs, dest } => Box::new(Ge { lhs, rhs, dest }),
            Op::And { lhs, rhs, dest } => Box::new(crate::sql::operations::And { lhs, rhs, dest }),
            Op::Or { lhs, rhs, dest } => Box::new(crate::sql::operations::Or { lhs, rhs, dest }),
            Op::Not { src, dest } => Box::new(Not { src, dest }),
            Op::Negate { src, dest } => Box::new(Negate { src, dest }),
            Op::Add { lhs, rhs, dest } => Box::new(Add { lhs, rhs, dest }),
            Op::Subtract { lhs, rhs, dest } => Box::new(Subtract { lhs, rhs, dest }),
            Op::Multiply { lhs, rhs, dest } => Box::new(Multiply { lhs, rhs, dest }),
            Op::Divide { lhs, rhs, dest } => Box::new(Divide { lhs, rhs, dest }),
            Op::ResultRow { fields } => Box::new(ResultRow { fields }),
            Op::Halt => Box::new(Halt),
        })
    }
}

fn resolve_label(labels: &HashMap<Label, usize>, label: Label) -> Result<usize, PlanError> {
    labels
        .get(&label)
        .copied()
        .ok_or_else(|| PlanError::new(format!("unresolved label {label}")))
}

fn literal_limit(expr: &Expr) -> Result<usize, PlanError> {
    match expr {
        Expr::Literal(Literal::Integer(value)) => usize::try_from(*value)
            .map_err(|_| PlanError::new("LIMIT must be a non-negative integer")),
        _ => Err(PlanError::new(
            "only integer literal LIMIT is supported by VM codegen",
        )),
    }
}

#[allow(dead_code)]
fn _uses_btree_data_type(_: BTreeDataType) {}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::sql::ast::TableName;
    use crate::sql::optimize::{PhysicalPlan, PhysicalPlanNode, PhysicalProperties};
    use crate::sql::parser::parse_sql;
    use crate::sql::relation::RelationExpr;
    use crate::sql::schema::ColumnDef;

    use super::*;

    #[derive(Default)]
    struct FakeCatalog {
        tables: HashMap<String, TableBinding>,
    }

    impl FakeCatalog {
        fn with_table(mut self, name: &str, root_pgno: u32, columns: &[&str]) -> Self {
            self.tables.insert(
                name.to_string(),
                TableBinding {
                    root_pgno,
                    columns: columns
                        .iter()
                        .map(|name| ColumnDef {
                            name: (*name).to_string(),
                            col_type: "TEXT".into(),
                            nullable: true,
                            is_primary_key: false,
                        })
                        .collect(),
                },
            );
            self
        }
    }

    impl Catalog for FakeCatalog {
        fn table(&self, name: &TableName) -> Option<TableBinding> {
            self.tables.get(&name.name).cloned()
        }
    }

    #[test]
    fn compiles_scan_filter_project() {
        let plan = plan_from_select("select id from users where age > 18");
        let catalog = FakeCatalog::default().with_table("users", 7, &["id", "age"]);

        let ops = compile_physical_plan(&plan, &catalog).expect("compile");
        let explains: Vec<String> = ops.iter().map(|op| op.explain()).collect();

        assert_eq!(explains[0], "OpenRead cursor=0 root=7");
        assert!(explains.iter().any(|op| op == "Rewind cursor=0"));
        assert!(explains.iter().any(|op| op.starts_with("Gt r")));
        assert!(explains.iter().any(|op| op.starts_with("IfNot r")));
        assert!(explains.iter().any(|op| op.starts_with("ResultRow [")));
        assert_eq!(explains.last().map(String::as_str), Some("Halt"));
    }

    #[test]
    fn compiles_limit_as_result_guard() {
        let plan = plan_from_select("select id from users limit 2");
        let catalog = FakeCatalog::default().with_table("users", 3, &["id"]);

        let ops = compile_physical_plan(&plan, &catalog).expect("compile");
        let explains: Vec<String> = ops.iter().map(|op| op.explain()).collect();

        assert!(explains
            .iter()
            .any(|op| op.starts_with("IfResultsGe 2 -> pc=")));
    }

    #[test]
    fn compiles_join_as_nested_loops() {
        let plan =
            plan_from_select("select u.id, o.id from users u join orders o on u.id = o.user_id");
        let catalog = FakeCatalog::default()
            .with_table("users", 3, &["id"])
            .with_table("orders", 4, &["id", "user_id"]);

        let ops = compile_physical_plan(&plan, &catalog).expect("compile");
        let explains: Vec<String> = ops.iter().map(|op| op.explain()).collect();

        assert!(explains.iter().any(|op| op == "OpenRead cursor=0 root=3"));
        assert!(explains.iter().any(|op| op == "OpenRead cursor=1 root=4"));
        assert!(explains.iter().any(|op| op.starts_with("Eq r")));
    }

    #[test]
    fn rejects_sort_until_vm_support_exists() {
        let input = plan_from_select("select id from users");
        let sorted = PhysicalPlan {
            node: PhysicalPlanNode::Sort {
                items: vec![order_item("select id from users order by id desc")],
                input: Box::new(input),
            },
            cost: 0.0,
            rows: 1.0,
            properties: PhysicalProperties::default(),
        };
        let catalog = FakeCatalog::default().with_table("users", 3, &["id"]);

        let err = match compile_physical_plan(&sorted, &catalog) {
            Ok(_) => panic!("sort unsupported"),
            Err(err) => err,
        };

        assert!(err.message.contains("Sort physical plan"));
    }

    fn plan_from_select(sql: &str) -> PhysicalPlan {
        let ast = parse_sql(sql).expect("parse");
        let select = match ast.statements.into_iter().next().expect("statement") {
            crate::sql::ast::Statement::Select(select) => select,
            _ => panic!("expected select"),
        };
        let relation = RelationExpr::from_select(&select).expect("relation");
        crate::sql::optimize::CascadesOptimizer::optimize_relation(&relation).expect("plan")
    }

    fn order_item(sql: &str) -> crate::sql::ast::OrderByItem {
        let ast = parse_sql(sql).expect("parse");
        match ast.statements.into_iter().next().expect("statement") {
            crate::sql::ast::Statement::Select(select) => select.order_by[0].clone(),
            _ => panic!("expected select"),
        }
    }
}
