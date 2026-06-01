use std::fmt::{Display, Formatter};

use crate::sql::ast::{
    Assignment, BinaryOp, ColumnRef, CreateTableStmt, DeleteStmt, DropTableStmt, Expr, InsertStmt,
    JoinClause, JoinConstraint, JoinType, Literal, OrderByItem, OrderDirection, RootNode,
    SelectItem, SelectItemKind, SelectStmt, Statement, TableName, TableRef, TransactionKind,
    TransactionStmt, UnaryOp, UpdateStmt,
};
use crate::sql::lexer::{lex_sqlexpr, TokenType};
use crate::sql::schema::ColumnDef;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub pos: usize,
    pub message: String,
}

impl ParseError {
    fn new(pos: usize, message: impl Into<String>) -> Self {
        Self {
            pos,
            message: message.into(),
        }
    }
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at token {}: {}", self.pos, self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse_sql(sql: &str) -> Result<RootNode, ParseError> {
    Parser::new(lex_sqlexpr(sql.as_bytes())).parse_root()
}

pub fn parse_tokens(tokens: Vec<TokenType>) -> Result<RootNode, ParseError> {
    Parser::new(tokens).parse_root()
}

#[derive(Clone, Debug)]
pub struct Parser {
    tokens: Vec<TokenType>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<TokenType>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse_root(&mut self) -> Result<RootNode, ParseError> {
        let mut statements = Vec::new();
        self.consume_semicolons();

        while !self.is_eof() {
            statements.push(self.parse_statement()?);
            if !self.consume_oper(";") && !self.is_eof() {
                return Err(self.error("expected ';' between statements"));
            }
            self.consume_semicolons();
        }

        Ok(RootNode::new(statements))
    }

    fn parse_statement(&mut self) -> Result<Statement, ParseError> {
        if self.consume_keyword("create") {
            self.parse_create()
        } else if self.consume_keyword("drop") {
            self.parse_drop()
        } else if self.consume_keyword("insert") {
            self.parse_insert()
        } else if self.consume_keyword("select") {
            self.parse_select()
        } else if self.consume_keyword("update") {
            self.parse_update()
        } else if self.consume_keyword("delete") {
            self.parse_delete()
        } else if self.consume_keyword("begin") {
            self.consume_keyword("transaction");
            Ok(Statement::Transaction(TransactionStmt {
                kind: TransactionKind::Begin,
            }))
        } else if self.consume_keyword("commit") {
            self.consume_keyword("transaction");
            Ok(Statement::Transaction(TransactionStmt {
                kind: TransactionKind::Commit,
            }))
        } else if self.consume_keyword("rollback") {
            self.consume_keyword("transaction");
            Ok(Statement::Transaction(TransactionStmt {
                kind: TransactionKind::Rollback,
            }))
        } else {
            Err(self.error("expected statement"))
        }
    }

    fn parse_create(&mut self) -> Result<Statement, ParseError> {
        self.expect_keyword("table")?;
        let name = self.parse_table_name()?;
        self.expect_oper("(")?;

        let mut columns = Vec::new();
        if !self.check_oper(")") {
            loop {
                columns.push(self.parse_column_def()?);
                if !self.consume_oper(",") {
                    break;
                }
            }
        }

        self.expect_oper(")")?;
        Ok(Statement::CreateTable(CreateTableStmt { name, columns }))
    }

    fn parse_column_def(&mut self) -> Result<ColumnDef, ParseError> {
        let name = self.expect_ident()?;
        let col_type = self.parse_type_name()?;
        let mut nullable = true;
        let mut is_primary_key = false;

        while !self.is_eof() && !self.check_oper(",") && !self.check_oper(")") {
            if self.consume_keyword("primary") {
                self.expect_keyword("key")?;
                is_primary_key = true;
                nullable = false;
            } else if self.consume_keyword("not") {
                self.expect_keyword("null")?;
                nullable = false;
            } else if self.consume_keyword("null") {
                nullable = true;
            } else {
                return Err(self.error("expected column constraint"));
            }
        }

        Ok(ColumnDef {
            name,
            col_type,
            nullable,
            is_primary_key,
        })
    }

    fn parse_type_name(&mut self) -> Result<String, ParseError> {
        let mut ty = self.expect_ident()?;
        if self.consume_oper("(") {
            let arg = self.expect_const_string()?;
            self.expect_oper(")")?;
            ty.push('(');
            ty.push_str(&arg);
            ty.push(')');
        }
        Ok(ty)
    }

    fn parse_drop(&mut self) -> Result<Statement, ParseError> {
        self.expect_keyword("table")?;
        let name = self.parse_table_name()?;
        Ok(Statement::DropTable(DropTableStmt { name }))
    }

    fn parse_insert(&mut self) -> Result<Statement, ParseError> {
        self.expect_keyword("into")?;
        let table = self.parse_table_name()?;
        let columns = if self.consume_oper("(") {
            let mut columns = Vec::new();
            if !self.check_oper(")") {
                loop {
                    columns.push(self.expect_ident()?);
                    if !self.consume_oper(",") {
                        break;
                    }
                }
            }
            self.expect_oper(")")?;
            Some(columns)
        } else {
            None
        };

        self.expect_keyword("values")?;
        let mut rows = Vec::new();
        loop {
            self.expect_oper("(")?;
            rows.push(self.parse_expr_list_until(")")?);
            self.expect_oper(")")?;
            if !self.consume_oper(",") {
                break;
            }
        }

        Ok(Statement::Insert(InsertStmt {
            table,
            columns,
            rows,
        }))
    }

    fn parse_select(&mut self) -> Result<Statement, ParseError> {
        let projection = self.parse_select_items()?;
        let from = if self.consume_keyword("from") {
            Some(self.parse_table_ref()?)
        } else {
            None
        };
        let where_clause = if self.consume_keyword("where") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let order_by = if self.consume_keyword("order") {
            self.expect_keyword("by")?;
            self.parse_order_by_items()?
        } else {
            Vec::new()
        };
        let limit = if self.consume_keyword("limit") {
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok(Statement::Select(SelectStmt {
            projection,
            from,
            where_clause,
            order_by,
            limit,
        }))
    }

    fn parse_select_items(&mut self) -> Result<Vec<SelectItem>, ParseError> {
        let mut items = Vec::new();
        loop {
            let kind = if self.consume_oper("*") {
                SelectItemKind::Wildcard
            } else {
                SelectItemKind::Expr(self.parse_expr()?)
            };
            let alias = if self.consume_keyword("as") {
                Some(self.expect_ident()?)
            } else if self.next_ident_is_alias() {
                Some(self.expect_ident()?)
            } else {
                None
            };
            items.push(SelectItem { kind, alias });

            if !self.consume_oper(",") {
                break;
            }
        }
        Ok(items)
    }

    fn parse_order_by_items(&mut self) -> Result<Vec<OrderByItem>, ParseError> {
        let mut items = Vec::new();
        loop {
            let expr = self.parse_expr()?;
            let direction = if self.consume_keyword("desc") {
                OrderDirection::Desc
            } else {
                self.consume_keyword("asc");
                OrderDirection::Asc
            };
            items.push(OrderByItem { expr, direction });

            if !self.consume_oper(",") {
                break;
            }
        }
        Ok(items)
    }

    fn parse_update(&mut self) -> Result<Statement, ParseError> {
        let table = self.parse_table_name()?;
        self.expect_keyword("set")?;

        let mut assignments = Vec::new();
        loop {
            let column = self.expect_ident()?;
            self.expect_oper("=")?;
            let value = self.parse_expr()?;
            assignments.push(Assignment { column, value });
            if !self.consume_oper(",") {
                break;
            }
        }

        let where_clause = if self.consume_keyword("where") {
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok(Statement::Update(UpdateStmt {
            table,
            assignments,
            where_clause,
        }))
    }

    fn parse_delete(&mut self) -> Result<Statement, ParseError> {
        self.expect_keyword("from")?;
        let table = self.parse_table_name()?;
        let where_clause = if self.consume_keyword("where") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Statement::Delete(DeleteStmt {
            table,
            where_clause,
        }))
    }

    fn parse_table_ref(&mut self) -> Result<TableRef, ParseError> {
        let mut table = self.parse_table_primary()?;

        while let Some(join_type) = self.parse_join_type()? {
            let joined_table = self.parse_table_primary()?;
            let constraint = if self.consume_keyword("on") {
                JoinConstraint::On(self.parse_expr()?)
            } else if self.consume_keyword("using") {
                self.expect_oper("(")?;
                let mut columns = Vec::new();
                if !self.check_oper(")") {
                    loop {
                        columns.push(self.expect_ident()?);
                        if !self.consume_oper(",") {
                            break;
                        }
                    }
                }
                self.expect_oper(")")?;
                JoinConstraint::Using(columns)
            } else {
                JoinConstraint::None
            };

            table.joins.push(JoinClause {
                join_type,
                table: joined_table,
                constraint,
            });
        }

        Ok(table)
    }

    fn parse_table_primary(&mut self) -> Result<TableRef, ParseError> {
        let name = self.parse_table_name()?;
        let alias = if self.consume_keyword("as") {
            Some(self.expect_ident()?)
        } else if self.next_ident_is_alias() {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(TableRef {
            name,
            alias,
            joins: Vec::new(),
        })
    }

    fn parse_join_type(&mut self) -> Result<Option<JoinType>, ParseError> {
        if self.consume_keyword("join") {
            return Ok(Some(JoinType::Inner));
        }

        if self.consume_keyword("inner") {
            self.expect_keyword("join")?;
            return Ok(Some(JoinType::Inner));
        }

        if self.consume_keyword("left") {
            self.consume_keyword("outer");
            self.expect_keyword("join")?;
            return Ok(Some(JoinType::Left));
        }

        if self.consume_keyword("right") {
            self.consume_keyword("outer");
            self.expect_keyword("join")?;
            return Ok(Some(JoinType::Right));
        }

        if self.consume_keyword("full") {
            self.consume_keyword("outer");
            self.expect_keyword("join")?;
            return Ok(Some(JoinType::Full));
        }

        if self.consume_keyword("cross") {
            self.expect_keyword("join")?;
            return Ok(Some(JoinType::Cross));
        }

        Ok(None)
    }

    fn parse_table_name(&mut self) -> Result<TableName, ParseError> {
        let first = self.expect_ident()?;
        if self.consume_oper(".") {
            let name = self.expect_ident()?;
            Ok(TableName {
                schema: Some(first),
                name,
            })
        } else {
            Ok(TableName::new(first))
        }
    }

    fn parse_expr_list_until(&mut self, end: &str) -> Result<Vec<Expr>, ParseError> {
        let mut exprs = Vec::new();
        if self.check_oper(end) {
            return Ok(exprs);
        }
        loop {
            exprs.push(self.parse_expr()?);
            if !self.consume_oper(",") {
                break;
            }
        }
        Ok(exprs)
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_and()?;
        while self.consume_keyword("or") {
            let right = self.parse_and()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op: BinaryOp::Or,
                right: Box::new(right),
            };
        }
        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_comparison()?;
        while self.consume_keyword("and") {
            let right = self.parse_comparison()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op: BinaryOp::And,
                right: Box::new(right),
            };
        }
        Ok(expr)
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_additive()?;
        while let Some(op) = self.consume_comparison_op() {
            let right = self.parse_additive()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
            };
        }
        Ok(expr)
    }

    fn parse_additive(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_multiplicative()?;
        loop {
            let op = if self.consume_oper("+") {
                Some(BinaryOp::Add)
            } else if self.consume_oper("-") {
                Some(BinaryOp::Subtract)
            } else {
                None
            };
            let Some(op) = op else {
                break;
            };
            let right = self.parse_multiplicative()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
            };
        }
        Ok(expr)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_unary()?;
        loop {
            let op = if self.consume_oper("*") {
                Some(BinaryOp::Multiply)
            } else if self.consume_oper("/") {
                Some(BinaryOp::Divide)
            } else {
                None
            };
            let Some(op) = op else {
                break;
            };
            let right = self.parse_unary()?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
            };
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        if self.consume_keyword("not") {
            Ok(Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(self.parse_unary()?),
            })
        } else if self.consume_oper("-") {
            Ok(Expr::Unary {
                op: UnaryOp::Negate,
                expr: Box::new(self.parse_unary()?),
            })
        } else if self.consume_oper("+") {
            Ok(Expr::Unary {
                op: UnaryOp::Positive,
                expr: Box::new(self.parse_unary()?),
            })
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        if self.consume_oper("(") {
            let expr = self.parse_expr()?;
            self.expect_oper(")")?;
            return Ok(expr);
        }

        if self.consume_keyword("null") {
            return Ok(Expr::Literal(Literal::Null));
        }

        if let Some(value) = self.consume_const() {
            if value.contains('.') {
                if let Ok(value) = value.parse::<f64>() {
                    return Ok(Expr::Literal(Literal::Real(value)));
                }
            }
            if let Ok(value) = value.parse::<i64>() {
                return Ok(Expr::Literal(Literal::Integer(value)));
            }
            return Ok(Expr::Literal(Literal::Text(value)));
        }

        let name = self.expect_ident()?;
        if self.consume_oper("(") {
            let args = self.parse_expr_list_until(")")?;
            self.expect_oper(")")?;
            return Ok(Expr::FunctionCall { name, args });
        }

        if self.consume_oper(".") {
            let col_name = self.expect_ident()?;
            Ok(Expr::Column(ColumnRef {
                table: Some(name),
                name: col_name,
            }))
        } else {
            Ok(Expr::Column(ColumnRef::new(name)))
        }
    }

    fn consume_comparison_op(&mut self) -> Option<BinaryOp> {
        if self.consume_oper("=") || self.consume_oper("==") {
            Some(BinaryOp::Eq)
        } else if self.consume_oper("!=") || self.consume_oper("<>") {
            Some(BinaryOp::Ne)
        } else if self.consume_oper("<=") {
            Some(BinaryOp::Le)
        } else if self.consume_oper(">=") {
            Some(BinaryOp::Ge)
        } else if self.consume_oper("<") {
            Some(BinaryOp::Lt)
        } else if self.consume_oper(">") {
            Some(BinaryOp::Gt)
        } else {
            None
        }
    }

    fn consume_semicolons(&mut self) {
        while self.consume_oper(";") {}
    }

    fn next_ident_is_alias(&self) -> bool {
        match self.peek() {
            Some(TokenType::Ident(bytes)) => {
                let ident = String::from_utf8_lossy(bytes);
                !is_clause_keyword(&ident)
            }
            _ => false,
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        match self.peek() {
            Some(TokenType::Ident(bytes)) if bytes.eq_ignore_ascii_case(keyword.as_bytes()) => {
                self.pos += 1;
                true
            }
            _ => false,
        }
    }

    fn expect_keyword(&mut self, keyword: &str) -> Result<(), ParseError> {
        if self.consume_keyword(keyword) {
            Ok(())
        } else {
            Err(self.error(format!("expected keyword '{keyword}'")))
        }
    }

    fn consume_oper(&mut self, oper: &str) -> bool {
        match self.peek() {
            Some(TokenType::Oper(bytes)) if bytes == oper.as_bytes() => {
                self.pos += 1;
                true
            }
            _ => false,
        }
    }

    fn check_oper(&self, oper: &str) -> bool {
        matches!(self.peek(), Some(TokenType::Oper(bytes)) if bytes == oper.as_bytes())
    }

    fn expect_oper(&mut self, oper: &str) -> Result<(), ParseError> {
        if self.consume_oper(oper) {
            Ok(())
        } else {
            Err(self.error(format!("expected operator '{oper}'")))
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Some(TokenType::Ident(bytes)) => {
                let ident = String::from_utf8_lossy(bytes).into_owned();
                self.pos += 1;
                Ok(ident)
            }
            _ => Err(self.error("expected identifier")),
        }
    }

    fn consume_const(&mut self) -> Option<String> {
        match self.peek() {
            Some(TokenType::Const(bytes)) => {
                let value = String::from_utf8_lossy(bytes).into_owned();
                self.pos += 1;
                Some(value)
            }
            _ => None,
        }
    }

    fn expect_const_string(&mut self) -> Result<String, ParseError> {
        self.consume_const()
            .ok_or_else(|| self.error("expected constant"))
    }

    fn peek(&self) -> Option<&TokenType> {
        self.tokens.get(self.pos)
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError::new(self.pos, message)
    }
}

fn is_clause_keyword(ident: &str) -> bool {
    matches!(
        ident.to_ascii_lowercase().as_str(),
        "from"
            | "where"
            | "order"
            | "by"
            | "limit"
            | "group"
            | "having"
            | "join"
            | "inner"
            | "left"
            | "right"
            | "full"
            | "cross"
            | "outer"
            | "on"
            | "using"
            | "set"
            | "values"
            | "asc"
            | "desc"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_table() {
        let ast = parse_sql(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);",
        )
        .expect("parse");

        let Statement::CreateTable(stmt) = &ast.statements[0] else {
            panic!("expected create table");
        };
        assert_eq!(stmt.name.name, "users");
        assert_eq!(stmt.columns.len(), 3);
        assert!(stmt.columns[0].is_primary_key);
        assert!(!stmt.columns[1].nullable);
    }

    #[test]
    fn parses_insert_rows() {
        let ast = parse_sql("insert into users (id, name) values (1, 'Ada'), (2, 'Linus')")
            .expect("parse");

        let Statement::Insert(stmt) = &ast.statements[0] else {
            panic!("expected insert");
        };
        assert_eq!(stmt.table.name, "users");
        assert_eq!(stmt.columns.as_ref().expect("columns"), &["id", "name"]);
        assert_eq!(stmt.rows.len(), 2);
        assert_eq!(stmt.rows[0][1], Expr::Literal(Literal::Text("Ada".into())));
    }

    #[test]
    fn parses_select_where_order_limit() {
        let ast = parse_sql(
            "select id, name as username from users u where age >= 18 and name != 'bot' order by id desc limit 10",
        )
        .expect("parse");

        let Statement::Select(stmt) = &ast.statements[0] else {
            panic!("expected select");
        };
        assert_eq!(stmt.projection.len(), 2);
        assert_eq!(
            stmt.from.as_ref().expect("from").alias.as_deref(),
            Some("u")
        );
        assert!(stmt.where_clause.is_some());
        assert_eq!(stmt.order_by[0].direction, OrderDirection::Desc);
        assert_eq!(stmt.limit, Some(Expr::Literal(Literal::Integer(10))));
    }

    #[test]
    fn parses_inner_join_with_on_clause() {
        let ast =
            parse_sql("select u.id from users u join orders o on u.id = o.user_id where o.id > 10")
                .expect("parse");

        let Statement::Select(stmt) = &ast.statements[0] else {
            panic!("expected select");
        };
        let from = stmt.from.as_ref().expect("from");
        assert_eq!(from.name.name, "users");
        assert_eq!(from.alias.as_deref(), Some("u"));
        assert_eq!(from.joins.len(), 1);

        let join = &from.joins[0];
        assert_eq!(join.join_type, JoinType::Inner);
        assert_eq!(join.table.name.name, "orders");
        assert_eq!(join.table.alias.as_deref(), Some("o"));
        assert_eq!(
            join.constraint,
            JoinConstraint::On(Expr::Binary {
                left: Box::new(Expr::Column(ColumnRef {
                    table: Some("u".into()),
                    name: "id".into(),
                })),
                op: BinaryOp::Eq,
                right: Box::new(Expr::Column(ColumnRef {
                    table: Some("o".into()),
                    name: "user_id".into(),
                })),
            })
        );
        assert!(stmt.where_clause.is_some());
    }

    #[test]
    fn parses_join_chain_and_using_clause() {
        let ast = parse_sql(
            "select * from users u left outer join orders o using (user_id) cross join regions r",
        )
        .expect("parse");

        let Statement::Select(stmt) = &ast.statements[0] else {
            panic!("expected select");
        };
        let from = stmt.from.as_ref().expect("from");
        assert_eq!(from.joins.len(), 2);

        assert_eq!(from.joins[0].join_type, JoinType::Left);
        assert_eq!(from.joins[0].table.name.name, "orders");
        assert_eq!(from.joins[0].table.alias.as_deref(), Some("o"));
        assert_eq!(
            from.joins[0].constraint,
            JoinConstraint::Using(vec!["user_id".into()])
        );

        assert_eq!(from.joins[1].join_type, JoinType::Cross);
        assert_eq!(from.joins[1].table.name.name, "regions");
        assert_eq!(from.joins[1].table.alias.as_deref(), Some("r"));
        assert_eq!(from.joins[1].constraint, JoinConstraint::None);
    }

    #[test]
    fn parses_update_delete_and_transaction() {
        let ast = parse_sql(
            "begin; update users set name = 'Grace', age = age + 1 where id = 1; delete from users where age > 100; rollback;",
        )
        .expect("parse");

        assert!(matches!(
            ast.statements[0],
            Statement::Transaction(TransactionStmt {
                kind: TransactionKind::Begin
            })
        ));
        assert!(matches!(ast.statements[1], Statement::Update(_)));
        assert!(matches!(ast.statements[2], Statement::Delete(_)));
        assert!(matches!(
            ast.statements[3],
            Statement::Transaction(TransactionStmt {
                kind: TransactionKind::Rollback
            })
        ));
    }

    #[test]
    fn expression_respects_arithmetic_precedence() {
        let expr = parse_single_expr("1 + 2 * 3").expect("parse expr");

        assert_eq!(
            expr,
            Expr::Binary {
                left: Box::new(Expr::Literal(Literal::Integer(1))),
                op: BinaryOp::Add,
                right: Box::new(Expr::Binary {
                    left: Box::new(Expr::Literal(Literal::Integer(2))),
                    op: BinaryOp::Multiply,
                    right: Box::new(Expr::Literal(Literal::Integer(3))),
                }),
            }
        );
    }

    #[test]
    fn expression_parentheses_override_precedence() {
        let expr = parse_single_expr("(1 + 2) * 3").expect("parse expr");

        assert_eq!(
            expr,
            Expr::Binary {
                left: Box::new(Expr::Binary {
                    left: Box::new(Expr::Literal(Literal::Integer(1))),
                    op: BinaryOp::Add,
                    right: Box::new(Expr::Literal(Literal::Integer(2))),
                }),
                op: BinaryOp::Multiply,
                right: Box::new(Expr::Literal(Literal::Integer(3))),
            }
        );
    }

    #[test]
    fn expression_parses_boolean_precedence() {
        let expr = parse_single_expr("a = 1 or b = 2 and not c").expect("parse expr");

        assert_eq!(
            expr,
            Expr::Binary {
                left: Box::new(Expr::Binary {
                    left: Box::new(Expr::Column(ColumnRef::new("a"))),
                    op: BinaryOp::Eq,
                    right: Box::new(Expr::Literal(Literal::Integer(1))),
                }),
                op: BinaryOp::Or,
                right: Box::new(Expr::Binary {
                    left: Box::new(Expr::Binary {
                        left: Box::new(Expr::Column(ColumnRef::new("b"))),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::Literal(Literal::Integer(2))),
                    }),
                    op: BinaryOp::And,
                    right: Box::new(Expr::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(Expr::Column(ColumnRef::new("c"))),
                    }),
                }),
            }
        );
    }

    #[test]
    fn expression_parses_qualified_columns_and_functions() {
        let expr = parse_single_expr("lower(users.name) != 'admin'").expect("parse expr");

        assert_eq!(
            expr,
            Expr::Binary {
                left: Box::new(Expr::FunctionCall {
                    name: "lower".into(),
                    args: vec![Expr::Column(ColumnRef {
                        table: Some("users".into()),
                        name: "name".into(),
                    })],
                }),
                op: BinaryOp::Ne,
                right: Box::new(Expr::Literal(Literal::Text("admin".into()))),
            }
        );
    }

    #[test]
    fn expression_parses_nested_unary() {
        let expr = parse_single_expr("-+age").expect("parse expr");

        assert_eq!(
            expr,
            Expr::Unary {
                op: UnaryOp::Negate,
                expr: Box::new(Expr::Unary {
                    op: UnaryOp::Positive,
                    expr: Box::new(Expr::Column(ColumnRef::new("age"))),
                }),
            }
        );
    }

    fn parse_single_expr(sql: &str) -> Result<Expr, ParseError> {
        let ast = parse_sql(&format!("select {sql}"))?;
        let Statement::Select(stmt) = ast.statements.into_iter().next().expect("one statement")
        else {
            panic!("expected select");
        };
        let SelectItemKind::Expr(expr) = stmt
            .projection
            .into_iter()
            .next()
            .expect("one projection")
            .kind
        else {
            panic!("expected expression projection");
        };
        Ok(expr)
    }
}
