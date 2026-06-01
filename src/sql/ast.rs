use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use crate::sql::operations::{
    Add, BeginTransaction, Close, Column, CommitTransaction, CreateTable, DeleteRow, Divide,
    DropTable, Eq, Ge, Goto, Gt, Halt, IfCursorEnd, IfNot, InsertRow, Integer, Le, Lt, MakeRecord,
    MoveNext, Multiply, Ne, Negate, NewRowid, Not, Null, OpenWrite, Or, Real, Rewind,
    RollbackTransaction, Rowid, Subtract, Text, UpdateRow, VMOperation,
};
use crate::sql::optimize::CascadesOptimizer;
use crate::sql::planner::{compile_physical_plan_body, Catalog, PlanError, TableBinding};
use crate::sql::relation::{RelationError, RelationExpr};
use crate::sql::schema::ColumnDef;

use super::operations::And;

pub trait ASTNode {
    fn to_oper<C: Catalog>(&self, catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

impl CodegenError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for CodegenError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "codegen error: {}", self.message)
    }
}

impl std::error::Error for CodegenError {}

impl From<RelationError> for CodegenError {
    fn from(value: RelationError) -> Self {
        Self::new(value.message)
    }
}

impl From<crate::sql::optimize::OptimizeError> for CodegenError {
    fn from(value: crate::sql::optimize::OptimizeError) -> Self {
        Self::new(value.message)
    }
}

impl From<PlanError> for CodegenError {
    fn from(value: PlanError) -> Self {
        Self::new(value.message)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RootNode {
    pub statements: Vec<Statement>,
}

impl RootNode {
    pub fn new(statements: Vec<Statement>) -> Self {
        Self { statements }
    }
}

impl ASTNode for RootNode {
    fn to_oper<C: Catalog>(&self, catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        let mut ops = Vec::new();
        for statement in &self.statements {
            ops.extend(statement.to_oper(catalog)?);
        }
        ops.push(Box::new(Halt));
        Ok(ops)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    CreateTable(CreateTableStmt),
    DropTable(DropTableStmt),
    Insert(InsertStmt),
    Select(SelectStmt),
    Update(UpdateStmt),
    Delete(DeleteStmt),
    Transaction(TransactionStmt),
}

impl ASTNode for Statement {
    fn to_oper<C: Catalog>(&self, catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        match self {
            Statement::CreateTable(stmt) => stmt.to_oper(catalog),
            Statement::DropTable(stmt) => stmt.to_oper(catalog),
            Statement::Insert(stmt) => stmt.to_oper(catalog),
            Statement::Select(stmt) => stmt.to_oper(catalog),
            Statement::Update(stmt) => stmt.to_oper(catalog),
            Statement::Delete(stmt) => stmt.to_oper(catalog),
            Statement::Transaction(stmt) => stmt.to_oper(catalog),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateTableStmt {
    pub name: TableName,
    pub columns: Vec<ColumnDef>,
}

impl ASTNode for CreateTableStmt {
    fn to_oper<C: Catalog>(&self, _catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        Ok(vec![Box::new(CreateTable {
            name: self.name.name.clone(),
            columns: self.columns.clone(),
        })])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DropTableStmt {
    pub name: TableName,
}

impl ASTNode for DropTableStmt {
    fn to_oper<C: Catalog>(&self, _catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        Ok(vec![Box::new(DropTable {
            name: self.name.name.clone(),
        })])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InsertStmt {
    pub table: TableName,
    pub columns: Option<Vec<String>>,
    pub rows: Vec<Vec<Expr>>,
}

impl ASTNode for InsertStmt {
    fn to_oper<C: Catalog>(&self, catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        MutationCompiler::new(catalog).compile_insert(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectStmt {
    pub projection: Vec<SelectItem>,
    pub from: Option<TableRef>,
    pub where_clause: Option<Expr>,
    pub order_by: Vec<OrderByItem>,
    pub limit: Option<Expr>,
}

impl ASTNode for SelectStmt {
    fn to_oper<C: Catalog>(&self, catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        let relation = RelationExpr::from_select(self)?;
        let plan = CascadesOptimizer::optimize_relation(&relation)?;
        compile_physical_plan_body(&plan, catalog).map_err(Into::into)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdateStmt {
    pub table: TableName,
    pub assignments: Vec<Assignment>,
    pub where_clause: Option<Expr>,
}

impl ASTNode for UpdateStmt {
    fn to_oper<C: Catalog>(&self, catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        MutationCompiler::new(catalog).compile_update(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeleteStmt {
    pub table: TableName,
    pub where_clause: Option<Expr>,
}

impl ASTNode for DeleteStmt {
    fn to_oper<C: Catalog>(&self, catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        MutationCompiler::new(catalog).compile_delete(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransactionStmt {
    pub kind: TransactionKind,
}

impl ASTNode for TransactionStmt {
    fn to_oper<C: Catalog>(&self, _catalog: &C) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        Ok(match self.kind {
            TransactionKind::Begin => vec![Box::new(BeginTransaction)],
            TransactionKind::Commit => vec![Box::new(CommitTransaction)],
            TransactionKind::Rollback => vec![Box::new(RollbackTransaction)],
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransactionKind {
    Begin,
    Commit,
    Rollback,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectItem {
    pub kind: SelectItemKind,
    pub alias: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SelectItemKind {
    Expr(Expr),
    Wildcard,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assignment {
    pub column: String,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderByItem {
    pub expr: Expr,
    pub direction: OrderDirection,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OrderDirection {
    Asc,
    Desc,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableRef {
    pub name: TableName,
    pub alias: Option<String>,
    pub joins: Vec<JoinClause>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinClause {
    pub join_type: JoinType,
    pub table: TableRef,
    pub constraint: JoinConstraint,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JoinConstraint {
    On(Expr),
    Using(Vec<String>),
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableName {
    pub schema: Option<String>,
    pub name: String,
}

impl TableName {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema: None,
            name: name.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnRef {
    pub table: Option<String>,
    pub name: String,
}

impl ColumnRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            table: None,
            name: name.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Literal(Literal),
    Column(ColumnRef),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    FunctionCall {
        name: String,
        args: Vec<Expr>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum UnaryOp {
    Not,
    Negate,
    Positive,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BinaryOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Add,
    Subtract,
    Multiply,
    Divide,
}

struct MutationCompiler<'a, C: Catalog> {
    catalog: &'a C,
    builder: CodegenBuilder,
    next_register: usize,
}

impl<'a, C: Catalog> MutationCompiler<'a, C> {
    const CURSOR: usize = 0;

    fn new(catalog: &'a C) -> Self {
        Self {
            catalog,
            builder: CodegenBuilder::new(),
            next_register: 0,
        }
    }

    fn compile_insert(
        mut self,
        stmt: &InsertStmt,
    ) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        let binding = self.table_binding(&stmt.table)?;
        self.builder.emit(CodegenOp::OpenWrite {
            cursor_id: Self::CURSOR,
            root_pgno: binding.root_pgno,
        });

        for row in &stmt.rows {
            let fields = self.compile_insert_fields(stmt, row, &binding.columns)?;
            let key = self.alloc_register();
            let record = self.alloc_register();
            self.builder.emit(CodegenOp::NewRowid {
                cursor_id: Self::CURSOR,
                dest: key,
            });
            self.builder.emit(CodegenOp::MakeRecord {
                dest: record,
                fields,
            });
            self.builder.emit(CodegenOp::InsertRow {
                cursor_id: Self::CURSOR,
                key_reg: key,
                value_reg: record,
            });
        }

        self.builder.emit(CodegenOp::Close {
            cursor_id: Self::CURSOR,
        });
        self.builder.finish()
    }

    fn compile_update(
        mut self,
        stmt: &UpdateStmt,
    ) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        let binding = self.table_binding(&stmt.table)?;
        let assignments = assignment_map(&stmt.assignments, &binding.columns)?;
        let table = TableContext {
            name: &stmt.table,
            columns: &binding.columns,
        };

        let loop_start = self.builder.new_label();
        let loop_end = self.builder.new_label();
        let skip_update = self.builder.new_label();

        self.builder.emit(CodegenOp::OpenWrite {
            cursor_id: Self::CURSOR,
            root_pgno: binding.root_pgno,
        });
        self.builder.emit(CodegenOp::Rewind {
            cursor_id: Self::CURSOR,
        });
        self.builder
            .emit(CodegenOp::IfCursorEnd { target: loop_end });
        self.builder.mark(loop_start);

        if let Some(predicate) = &stmt.where_clause {
            let cond = self.compile_expr(predicate, ExprEnv::SingleTable(table))?;
            self.builder.emit(CodegenOp::IfNot {
                cond_reg: cond,
                target: skip_update,
            });
        }

        let key = self.alloc_register();
        self.builder.emit(CodegenOp::Rowid {
            cursor_id: Self::CURSOR,
            dest: key,
        });

        let mut fields = Vec::with_capacity(binding.columns.len());
        for (col_idx, assignment) in assignments.iter().enumerate() {
            let value = match assignment {
                Some(expr) => self.compile_expr(expr, ExprEnv::SingleTable(table))?,
                None => {
                    let dest = self.alloc_register();
                    self.builder.emit(CodegenOp::Column {
                        cursor_id: Self::CURSOR,
                        col_idx,
                        dest,
                    });
                    dest
                }
            };
            fields.push(value);
        }

        let record = self.alloc_register();
        self.builder.emit(CodegenOp::MakeRecord {
            dest: record,
            fields,
        });
        self.builder.emit(CodegenOp::UpdateRow {
            cursor_id: Self::CURSOR,
            key_reg: key,
            value_reg: record,
        });

        self.builder.mark(skip_update);
        self.builder.emit(CodegenOp::MoveNext {
            cursor_id: Self::CURSOR,
        });
        self.builder
            .emit(CodegenOp::IfCursorEnd { target: loop_end });
        self.builder.emit(CodegenOp::Goto { target: loop_start });
        self.builder.mark(loop_end);
        self.builder.emit(CodegenOp::Close {
            cursor_id: Self::CURSOR,
        });
        self.builder.finish()
    }

    fn compile_delete(
        mut self,
        stmt: &DeleteStmt,
    ) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        let binding = self.table_binding(&stmt.table)?;
        let table = TableContext {
            name: &stmt.table,
            columns: &binding.columns,
        };

        let loop_start = self.builder.new_label();
        let loop_end = self.builder.new_label();
        let skip_delete = self.builder.new_label();

        self.builder.emit(CodegenOp::OpenWrite {
            cursor_id: Self::CURSOR,
            root_pgno: binding.root_pgno,
        });
        self.builder.emit(CodegenOp::Rewind {
            cursor_id: Self::CURSOR,
        });
        self.builder
            .emit(CodegenOp::IfCursorEnd { target: loop_end });
        self.builder.mark(loop_start);

        if let Some(predicate) = &stmt.where_clause {
            let cond = self.compile_expr(predicate, ExprEnv::SingleTable(table))?;
            self.builder.emit(CodegenOp::IfNot {
                cond_reg: cond,
                target: skip_delete,
            });
        }

        let key = self.alloc_register();
        self.builder.emit(CodegenOp::Rowid {
            cursor_id: Self::CURSOR,
            dest: key,
        });
        self.builder.emit(CodegenOp::DeleteRow {
            cursor_id: Self::CURSOR,
            key_reg: key,
        });

        self.builder.mark(skip_delete);
        self.builder.emit(CodegenOp::MoveNext {
            cursor_id: Self::CURSOR,
        });
        self.builder
            .emit(CodegenOp::IfCursorEnd { target: loop_end });
        self.builder.emit(CodegenOp::Goto { target: loop_start });
        self.builder.mark(loop_end);
        self.builder.emit(CodegenOp::Close {
            cursor_id: Self::CURSOR,
        });
        self.builder.finish()
    }

    fn compile_insert_fields(
        &mut self,
        stmt: &InsertStmt,
        row: &[Expr],
        columns: &[ColumnDef],
    ) -> Result<Vec<usize>, CodegenError> {
        let mut fields = Vec::with_capacity(columns.len());

        if let Some(insert_columns) = &stmt.columns {
            if insert_columns.len() != row.len() {
                return Err(CodegenError::new(format!(
                    "INSERT has {} columns but {} values",
                    insert_columns.len(),
                    row.len()
                )));
            }

            let mut values_by_column = vec![None; columns.len()];
            for (name, expr) in insert_columns.iter().zip(row.iter()) {
                let idx = find_column(columns, name)
                    .ok_or_else(|| CodegenError::new(format!("unknown column '{name}'")))?;
                if values_by_column[idx].is_some() {
                    return Err(CodegenError::new(format!(
                        "duplicate INSERT column '{name}'"
                    )));
                }
                values_by_column[idx] = Some(expr);
            }

            for expr in values_by_column {
                fields.push(match expr {
                    Some(expr) => self.compile_expr(expr, ExprEnv::NoColumns)?,
                    None => {
                        let dest = self.alloc_register();
                        self.builder.emit(CodegenOp::Null { dest });
                        dest
                    }
                });
            }
        } else {
            if row.len() != columns.len() {
                return Err(CodegenError::new(format!(
                    "INSERT has {} values but table '{}' has {} columns",
                    row.len(),
                    stmt.table.name,
                    columns.len()
                )));
            }
            for expr in row {
                fields.push(self.compile_expr(expr, ExprEnv::NoColumns)?);
            }
        }

        Ok(fields)
    }

    fn compile_expr(&mut self, expr: &Expr, env: ExprEnv<'_>) -> Result<usize, CodegenError> {
        match expr {
            Expr::Literal(literal) => Ok(self.compile_literal(literal)),
            Expr::Column(column) => self.compile_column(column, env),
            Expr::Unary { op, expr } => {
                let src = self.compile_expr(expr, env)?;
                match op {
                    UnaryOp::Positive => Ok(src),
                    UnaryOp::Not => {
                        let dest = self.alloc_register();
                        self.builder.emit(CodegenOp::Not { src, dest });
                        Ok(dest)
                    }
                    UnaryOp::Negate => {
                        let dest = self.alloc_register();
                        self.builder.emit(CodegenOp::Negate { src, dest });
                        Ok(dest)
                    }
                }
            }
            Expr::Binary { left, op, right } => {
                let lhs = self.compile_expr(left, env)?;
                let rhs = self.compile_expr(right, env)?;
                let dest = self.alloc_register();
                match op {
                    BinaryOp::Eq => self.builder.emit(CodegenOp::Eq { lhs, rhs, dest }),
                    BinaryOp::Ne => self.builder.emit(CodegenOp::Ne { lhs, rhs, dest }),
                    BinaryOp::Lt => self.builder.emit(CodegenOp::Lt { lhs, rhs, dest }),
                    BinaryOp::Le => self.builder.emit(CodegenOp::Le { lhs, rhs, dest }),
                    BinaryOp::Gt => self.builder.emit(CodegenOp::Gt { lhs, rhs, dest }),
                    BinaryOp::Ge => self.builder.emit(CodegenOp::Ge { lhs, rhs, dest }),
                    BinaryOp::And => self.builder.emit(CodegenOp::And { lhs, rhs, dest }),
                    BinaryOp::Or => self.builder.emit(CodegenOp::Or { lhs, rhs, dest }),
                    BinaryOp::Add => self.builder.emit(CodegenOp::Add { lhs, rhs, dest }),
                    BinaryOp::Subtract => self.builder.emit(CodegenOp::Subtract { lhs, rhs, dest }),
                    BinaryOp::Multiply => self.builder.emit(CodegenOp::Multiply { lhs, rhs, dest }),
                    BinaryOp::Divide => self.builder.emit(CodegenOp::Divide { lhs, rhs, dest }),
                }
                Ok(dest)
            }
            Expr::FunctionCall { name, .. } => Err(CodegenError::new(format!(
                "function '{name}' is not supported by VM codegen yet"
            ))),
        }
    }

    fn compile_literal(&mut self, literal: &Literal) -> usize {
        let dest = self.alloc_register();
        match literal {
            Literal::Null => self.builder.emit(CodegenOp::Null { dest }),
            Literal::Integer(value) => self.builder.emit(CodegenOp::Integer {
                dest,
                value: *value,
            }),
            Literal::Real(value) => self.builder.emit(CodegenOp::Real {
                dest,
                value: *value,
            }),
            Literal::Text(value) => self.builder.emit(CodegenOp::Text {
                dest,
                value: value.as_bytes().to_vec(),
            }),
            Literal::Blob(value) => self.builder.emit(CodegenOp::Text {
                dest,
                value: value.clone(),
            }),
        }
        dest
    }

    fn compile_column(
        &mut self,
        column: &ColumnRef,
        env: ExprEnv<'_>,
    ) -> Result<usize, CodegenError> {
        let table = match env {
            ExprEnv::SingleTable(table) => table,
            ExprEnv::NoColumns => {
                return Err(CodegenError::new(format!(
                    "column '{}' is not available in this expression",
                    column.name
                )))
            }
        };

        if let Some(qualifier) = &column.table {
            if !qualifier.eq_ignore_ascii_case(&table.name.name) {
                return Err(CodegenError::new(format!(
                    "unknown table qualifier '{qualifier}'"
                )));
            }
        }

        let col_idx = find_column(table.columns, &column.name)
            .ok_or_else(|| CodegenError::new(format!("unknown column '{}'", column.name)))?;
        let dest = self.alloc_register();
        self.builder.emit(CodegenOp::Column {
            cursor_id: Self::CURSOR,
            col_idx,
            dest,
        });
        Ok(dest)
    }

    fn table_binding(&self, table: &TableName) -> Result<TableBinding, CodegenError> {
        self.catalog
            .table(table)
            .ok_or_else(|| CodegenError::new(format!("unknown table '{}'", table.name)))
    }

    fn alloc_register(&mut self) -> usize {
        let register = self.next_register;
        self.next_register += 1;
        register
    }
}

#[derive(Clone, Copy)]
enum ExprEnv<'a> {
    NoColumns,
    SingleTable(TableContext<'a>),
}

#[derive(Clone, Copy)]
struct TableContext<'a> {
    name: &'a TableName,
    columns: &'a [ColumnDef],
}

fn assignment_map<'a>(
    assignments: &'a [Assignment],
    columns: &[ColumnDef],
) -> Result<Vec<Option<&'a Expr>>, CodegenError> {
    let mut result = vec![None; columns.len()];
    for assignment in assignments {
        let idx = find_column(columns, &assignment.column)
            .ok_or_else(|| CodegenError::new(format!("unknown column '{}'", assignment.column)))?;
        if result[idx].is_some() {
            return Err(CodegenError::new(format!(
                "duplicate assignment to column '{}'",
                assignment.column
            )));
        }
        result[idx] = Some(&assignment.value);
    }
    Ok(result)
}

fn find_column(columns: &[ColumnDef], name: &str) -> Option<usize> {
    columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case(name))
}

type Label = usize;

#[derive(Clone, Debug)]
enum CodegenOp {
    OpenWrite {
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
    Rowid {
        cursor_id: usize,
        dest: usize,
    },
    NewRowid {
        cursor_id: usize,
        dest: usize,
    },
    MakeRecord {
        dest: usize,
        fields: Vec<usize>,
    },
    InsertRow {
        cursor_id: usize,
        key_reg: usize,
        value_reg: usize,
    },
    DeleteRow {
        cursor_id: usize,
        key_reg: usize,
    },
    UpdateRow {
        cursor_id: usize,
        key_reg: usize,
        value_reg: usize,
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
}

#[derive(Clone, Debug, Default)]
struct CodegenBuilder {
    ops: Vec<CodegenOp>,
    labels: HashMap<Label, usize>,
    next_label: Label,
}

impl CodegenBuilder {
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

    fn emit(&mut self, op: CodegenOp) {
        self.ops.push(op);
    }

    fn finish(self) -> Result<Vec<Box<dyn VMOperation>>, CodegenError> {
        self.ops
            .into_iter()
            .map(|op| op.into_vm_operation(&self.labels))
            .collect()
    }
}

impl CodegenOp {
    fn into_vm_operation(
        self,
        labels: &HashMap<Label, usize>,
    ) -> Result<Box<dyn VMOperation>, CodegenError> {
        Ok(match self {
            CodegenOp::OpenWrite {
                cursor_id,
                root_pgno,
            } => Box::new(OpenWrite {
                cursor_id,
                root_pgno,
            }),
            CodegenOp::Close { cursor_id } => Box::new(Close { cursor_id }),
            CodegenOp::Rewind { cursor_id } => Box::new(Rewind { cursor_id }),
            CodegenOp::MoveNext { cursor_id } => Box::new(MoveNext { cursor_id }),
            CodegenOp::IfCursorEnd { target } => Box::new(IfCursorEnd {
                target: resolve_codegen_label(labels, target)?,
            }),
            CodegenOp::Goto { target } => Box::new(Goto {
                target: resolve_codegen_label(labels, target)?,
            }),
            CodegenOp::IfNot { cond_reg, target } => Box::new(IfNot {
                cond_reg,
                target: resolve_codegen_label(labels, target)?,
            }),
            CodegenOp::Integer { dest, value } => Box::new(Integer { dest, value }),
            CodegenOp::Real { dest, value } => Box::new(Real { dest, value }),
            CodegenOp::Text { dest, value } => Box::new(Text { dest, value }),
            CodegenOp::Null { dest } => Box::new(Null { dest }),
            CodegenOp::Column {
                cursor_id,
                col_idx,
                dest,
            } => Box::new(Column {
                cursor_id,
                col_idx,
                dest,
            }),
            CodegenOp::Rowid { cursor_id, dest } => Box::new(Rowid { cursor_id, dest }),
            CodegenOp::NewRowid { cursor_id, dest } => Box::new(NewRowid { cursor_id, dest }),
            CodegenOp::MakeRecord { dest, fields } => Box::new(MakeRecord { dest, fields }),
            CodegenOp::InsertRow {
                cursor_id,
                key_reg,
                value_reg,
            } => Box::new(InsertRow {
                cursor_id,
                key_reg,
                value_reg,
            }),
            CodegenOp::DeleteRow { cursor_id, key_reg } => {
                Box::new(DeleteRow { cursor_id, key_reg })
            }
            CodegenOp::UpdateRow {
                cursor_id,
                key_reg,
                value_reg,
            } => Box::new(UpdateRow {
                cursor_id,
                key_reg,
                value_reg,
            }),
            CodegenOp::Eq { lhs, rhs, dest } => Box::new(Eq { lhs, rhs, dest }),
            CodegenOp::Ne { lhs, rhs, dest } => Box::new(Ne { lhs, rhs, dest }),
            CodegenOp::Lt { lhs, rhs, dest } => Box::new(Lt { lhs, rhs, dest }),
            CodegenOp::Le { lhs, rhs, dest } => Box::new(Le { lhs, rhs, dest }),
            CodegenOp::Gt { lhs, rhs, dest } => Box::new(Gt { lhs, rhs, dest }),
            CodegenOp::Ge { lhs, rhs, dest } => Box::new(Ge { lhs, rhs, dest }),
            CodegenOp::And { lhs, rhs, dest } => Box::new(And { lhs, rhs, dest }),
            CodegenOp::Or { lhs, rhs, dest } => Box::new(Or { lhs, rhs, dest }),
            CodegenOp::Not { src, dest } => Box::new(Not { src, dest }),
            CodegenOp::Negate { src, dest } => Box::new(Negate { src, dest }),
            CodegenOp::Add { lhs, rhs, dest } => Box::new(Add { lhs, rhs, dest }),
            CodegenOp::Subtract { lhs, rhs, dest } => Box::new(Subtract { lhs, rhs, dest }),
            CodegenOp::Multiply { lhs, rhs, dest } => Box::new(Multiply { lhs, rhs, dest }),
            CodegenOp::Divide { lhs, rhs, dest } => Box::new(Divide { lhs, rhs, dest }),
        })
    }
}

fn resolve_codegen_label(
    labels: &HashMap<Label, usize>,
    label: Label,
) -> Result<usize, CodegenError> {
    labels
        .get(&label)
        .copied()
        .ok_or_else(|| CodegenError::new(format!("unresolved label {label}")))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::sql::parser::parse_sql;
    use crate::sql::planner::{Catalog, TableBinding};

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
    fn root_codegen_adds_single_halt_after_select_body() {
        let ast = parse_sql("select id from users where age > 18").expect("parse");
        let catalog = FakeCatalog::default().with_table("users", 7, &["id", "age"]);

        let explains = explain(ast.to_oper(&catalog).expect("codegen"));

        assert_eq!(explains[0], "OpenRead cursor=0 root=7");
        assert!(explains.iter().any(|op| op.starts_with("Gt r")));
        assert!(explains.iter().any(|op| op.starts_with("ResultRow [")));
        assert_eq!(explains.last().map(String::as_str), Some("Halt"));
        assert_eq!(explains.iter().filter(|op| *op == "Halt").count(), 1);
    }

    #[test]
    fn insert_codegen_maps_column_list_and_defaults_missing_columns_to_null() {
        let ast = parse_sql("insert into users (id, name) values (1, 'Ada')").expect("parse");
        let catalog = FakeCatalog::default().with_table("users", 3, &["id", "name", "age"]);

        let explains = explain(ast.statements[0].to_oper(&catalog).expect("codegen"));

        assert_eq!(explains[0], "OpenWrite cursor=0 root=3");
        assert!(explains.iter().any(|op| op == "Integer 1 -> r0"));
        assert!(explains.iter().any(|op| op == "Text len=3 -> r1"));
        assert!(explains.iter().any(|op| op == "Null -> r2"));
        assert!(explains
            .iter()
            .any(|op| op.starts_with("NewRowid cursor=0")));
        assert!(explains
            .iter()
            .any(|op| op.starts_with("InsertRow cursor=0")));
        assert_eq!(explains.last().map(String::as_str), Some("Close cursor=0"));
    }

    #[test]
    fn update_codegen_scans_filters_and_rewrites_record() {
        let ast = parse_sql("update users set age = age + 1 where id = 1").expect("parse");
        let catalog = FakeCatalog::default().with_table("users", 3, &["id", "age"]);

        let explains = explain(ast.statements[0].to_oper(&catalog).expect("codegen"));

        assert!(explains.iter().any(|op| op == "OpenWrite cursor=0 root=3"));
        assert!(explains.iter().any(|op| op.starts_with("Eq r")));
        assert!(explains.iter().any(|op| op.starts_with("IfNot r")));
        assert!(explains.iter().any(|op| op.starts_with("Add r")));
        assert!(explains.iter().any(|op| op.starts_with("Rowid cursor=0")));
        assert!(explains
            .iter()
            .any(|op| op.starts_with("UpdateRow cursor=0")));
    }

    #[test]
    fn delete_codegen_scans_filters_and_deletes_by_rowid() {
        let ast = parse_sql("delete from users where age > 100").expect("parse");
        let catalog = FakeCatalog::default().with_table("users", 3, &["id", "age"]);

        let explains = explain(ast.statements[0].to_oper(&catalog).expect("codegen"));

        assert!(explains.iter().any(|op| op == "OpenWrite cursor=0 root=3"));
        assert!(explains.iter().any(|op| op.starts_with("Gt r")));
        assert!(explains.iter().any(|op| op.starts_with("Rowid cursor=0")));
        assert!(explains
            .iter()
            .any(|op| op.starts_with("DeleteRow cursor=0")));
    }

    #[test]
    fn insert_codegen_rejects_unknown_column() {
        let ast = parse_sql("insert into users (missing) values (1)").expect("parse");
        let catalog = FakeCatalog::default().with_table("users", 3, &["id"]);

        let err = match ast.statements[0].to_oper(&catalog) {
            Ok(_) => panic!("expected codegen error"),
            Err(err) => err,
        };

        assert!(err.message.contains("unknown column"));
    }

    fn explain(ops: Vec<Box<dyn VMOperation>>) -> Vec<String> {
        ops.into_iter().map(|op| op.explain()).collect()
    }
}
