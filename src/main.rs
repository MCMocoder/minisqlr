use std::cell::Cell;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::rc::Rc;

use engine::{btree::BTree, pages::Pager};
use sql::ast::{ASTNode, Statement, TransactionKind};
use sql::parser::parse_sql;
use sql::schema::{SchemaManager, SchemaType};
use sql::vm::VMInstance;

mod engine;
mod sql;

fn main() {
    let dbname = env::args()
        .nth(1)
        .unwrap_or_else(|| "minisqlr.db".to_string());

    match Shell::open(PathBuf::from(dbname)) {
        Ok(mut shell) => shell.repl(),
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}

struct Shell {
    dbname: PathBuf,
    vm: VMInstance,
    explicit_transaction: Cell<bool>,
}

impl Shell {
    fn open(dbname: PathBuf) -> Result<Self, String> {
        if let Some(parent) = dbname.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| {
                    format!("cannot create directory '{}': {err}", parent.display())
                })?;
            }
        }

        let is_new = !dbname.exists();
        let dbname_string = dbname.to_string_lossy().into_owned();
        let db = Pager::create_or_opendb(dbname_string.clone());
        let pager = Rc::new(Pager::new(db, dbname_string));
        let btree = Rc::new(BTree::new(&pager, 2));

        if is_new {
            btree.create_db();
            btree.end_transaction_write();
        } else {
            btree.begin_transaction_read();
            btree.end_transaction_read();
        }

        Ok(Self {
            dbname,
            vm: VMInstance::new(btree),
            explicit_transaction: Cell::new(false),
        })
    }

    fn repl(&mut self) {
        println!("minisqlr {}", env!("CARGO_PKG_VERSION"));
        println!("Connected to {}", self.dbname.display());
        println!("Use .help for help. End SQL with ';'.");

        let stdin = io::stdin();
        let mut buffer = String::new();

        loop {
            let prompt = if buffer.trim().is_empty() {
                "minisqlr> "
            } else {
                "      ... "
            };
            print!("{prompt}");
            let _ = io::stdout().flush();

            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(err) => {
                    eprintln!("read error: {err}");
                    break;
                }
            }

            let trimmed = line.trim();
            if buffer.trim().is_empty() && trimmed.starts_with('.') {
                if !self.handle_meta(trimmed) {
                    break;
                }
                continue;
            }

            buffer.push_str(&line);
            if !sql_is_complete(&buffer) {
                continue;
            }

            let sql = std::mem::take(&mut buffer);
            if let Err(err) = self.execute_sql(&sql) {
                eprintln!("error: {err}");
            }
        }
    }

    fn handle_meta(&mut self, command: &str) -> bool {
        let mut parts = command.split_whitespace();
        match parts.next().unwrap_or_default() {
            ".exit" | ".quit" => false,
            ".help" => {
                print_help();
                true
            }
            ".open" => {
                let Some(path) = parts.next() else {
                    eprintln!("usage: .open FILE");
                    return true;
                };
                match Shell::open(PathBuf::from(path)) {
                    Ok(shell) => {
                        *self = shell;
                        println!("Connected to {}", self.dbname.display());
                    }
                    Err(err) => eprintln!("error: {err}"),
                }
                true
            }
            ".tables" => {
                let schema = self.vm.schema.borrow();
                let names = schema.table_names();
                if !names.is_empty() {
                    println!("{}", names.join(" "));
                }
                true
            }
            ".schema" => {
                self.print_schema(parts.next());
                true
            }
            ".dbinfo" => {
                println!("file: {}", self.dbname.display());
                true
            }
            unknown => {
                eprintln!("unknown command: {unknown}");
                true
            }
        }
    }

    fn print_schema(&self, table: Option<&str>) {
        let schema = self.vm.schema.borrow();
        let mut rows = schema.list_schema();
        rows.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

        for info in rows {
            if !matches!(info.schema_type, SchemaType::Table) {
                continue;
            }
            if let Some(table) = table {
                if !info.name.eq_ignore_ascii_case(table) {
                    continue;
                }
            }

            let Some(structure) = SchemaManager::parse_structure(&info) else {
                continue;
            };
            let columns = structure
                .columns
                .iter()
                .map(|column| {
                    let mut text = format!("{} {}", column.name, column.col_type);
                    if column.is_primary_key {
                        text.push_str(" PRIMARY KEY");
                    } else if !column.nullable {
                        text.push_str(" NOT NULL");
                    }
                    text
                })
                .collect::<Vec<_>>()
                .join(", ");
            println!("CREATE TABLE {} ({});", info.name, columns);
        }
    }

    fn execute_sql(&self, sql: &str) -> Result<(), String> {
        let root = parse_sql(sql).map_err(|err| err.to_string())?;

        for statement in root.statements {
            self.execute_statement(statement)?;
        }

        Ok(())
    }

    fn execute_statement(&self, statement: Statement) -> Result<(), String> {
        let auto_transaction =
            !self.explicit_transaction.get() && !matches!(statement, Statement::Transaction(_));

        if auto_transaction {
            self.vm.btree.begin_transaction_write();
        }

        self.vm.reset();
        let ops = {
            let schema = self.vm.schema.borrow();
            statement.to_oper(&*schema).map_err(|err| err.to_string())?
        };

        let run_result = self.vm.run(&ops).map_err(|err| err.to_string());
        match (&statement, &run_result) {
            (Statement::Transaction(tx), Ok(_)) => match tx.kind {
                TransactionKind::Begin => self.explicit_transaction.set(true),
                TransactionKind::Commit | TransactionKind::Rollback => {
                    self.explicit_transaction.set(false)
                }
            },
            _ => {}
        }

        if auto_transaction {
            if run_result.is_ok() {
                self.vm.btree.end_transaction_write();
            } else {
                self.vm.btree.rollback();
            }
        }

        run_result?;
        self.print_results();
        Ok(())
    }

    fn print_results(&self) {
        let results = self.vm.results.borrow();
        for row in results.iter() {
            let fields = row.iter().map(ToString::to_string).collect::<Vec<_>>();
            println!("{}", fields.join("\t"));
        }
    }
}

fn sql_is_complete(sql: &str) -> bool {
    sql.trim_end().ends_with(';')
}

fn print_help() {
    println!(".open FILE     open or create a database file");
    println!(".tables        list tables");
    println!(".schema [T]    show CREATE TABLE statements");
    println!(".dbinfo        show current database file");
    println!(".exit          exit minisqlr");
}
