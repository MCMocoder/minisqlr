# minisqlr

**minisqlr** is a modern educational database written in Rust.

The project is intentionally small enough to read, but it is shaped like a real
database system: SQL is parsed into an AST, lowered into relational algebra,
optimized into a physical plan, compiled into virtual-machine operations, and
executed on top of a page-based B-tree storage engine.

It is not trying to be SQLite-compatible yet. The goal is to make the major
ideas behind a relational database visible and hackable.

## What Is Inside

- A page-oriented storage layer with a pager, rollback journal, and B-tree.
- A schema table similar in spirit to `sqlite_schema`.
- A SQL lexer and recursive-descent parser.
- AST nodes for common statements:
  - `CREATE TABLE`
  - `DROP TABLE`
  - `INSERT`
  - `SELECT`
  - `UPDATE`
  - `DELETE`
  - transaction statements
- Relational algebra lowering for `SELECT`.
- A Cascades-style optimizer with memo groups, physical properties, enforcers,
  cost estimation, and pruning.
- A register/cursor-based virtual machine.
- VM code generation for scans, filters, projection, joins, inserts, updates,
  deletes, and expression evaluation.
- A small interactive command-line shell.

## What Makes minisqlr Different

minisqlr is designed as a bridge between "toy database" projects and the ideas
used in real relational engines.

Many educational databases focus on one layer: a SQL parser, a B-tree, a
key-value store, or a simple executor. minisqlr tries to keep the whole database
pipeline in one readable codebase:

```text
SQL syntax
  -> logical algebra
  -> cost-based optimization
  -> bytecode-style execution
  -> transactional page storage
```

That gives the project a few unusual teaching properties:

- **Modern query planning in a small system.** The optimizer is not just a set
  of hard-coded rewrites. It uses a Cascades-style memo, required physical
  properties, enforcers, costs, and pruning. This makes it a compact place to
  study how industrial optimizers think without needing an industrial codebase.
- **SQLite-inspired execution, not direct interpretation.** SQL statements are
  compiled into VM operations. Cursors, registers, jumps, row construction, and
  result rows are visible as explicit instructions.
- **Relational algebra is a real intermediate layer.** `SELECT` is lowered into
  relational operators before optimization. This keeps parsing, planning, and
  execution separated in a way that mirrors larger systems.
- **Storage is part of the lesson.** The project includes a pager, rollback
  journal, schema table, B-tree nodes, and cursors instead of delegating storage
  to an external embedded database.
- **It is intentionally incomplete in useful places.** Missing features such as
  sort execution, index scans, statistics, and index maintenance are not hidden;
  they are clear extension points for learning.
- **The code is meant to be read forward.** Each layer exposes the next one:
  AST nodes generate plans, plans generate VM operations, and VM operations
  manipulate cursors over B-trees.

In short, minisqlr is less a clone of SQLite and more a small laboratory for
database architecture: modern optimizer ideas on top of a concrete storage
engine, with enough SQL surface to make the pipeline feel real.

## Quick Start

Build and test:

```sh
cargo test
```

Open or create a database file:

```sh
cargo run -- minisqlr.db
```

Then run SQL:

```sql
create table users (id integer, name text);
insert into users (id, name) values (1, 'Ada'), (2, 'Linus');
select id, name from users;
```

Shell commands:

```text
.help          show shell help
.open FILE    open or create another database file
.tables       list tables
.schema       show table schemas
.schema NAME  show one table schema
.dbinfo       show the current database file
.exit         quit
```

## Architecture

The current SQL pipeline is:

```text
SQL text
  -> lexer
  -> recursive-descent parser
  -> AST
  -> relational algebra
  -> Cascades optimizer
  -> physical plan
  -> VM operations
  -> B-tree storage
```

The storage layer is intentionally close to the concepts used by classic
embedded databases:

```text
Pager
  -> pages
  -> rollback journal
  -> B-tree nodes
  -> B-tree cursors
```

The VM is cursor-based. A typical query uses operations such as:

```text
OpenRead
Rewind
Column
Eq / Lt / Gt / And / Or
ResultRow
MoveNext
Close
```

This mirrors the broad shape of SQLite's VDBE, but with far fewer opcodes.

## SQL Support

The parser currently supports a useful teaching subset:

```sql
create table users (
  id integer primary key,
  name text not null,
  age integer
);

insert into users (id, name, age) values (1, 'Ada', 37);

select id, name
from users
where age >= 18
order by id desc
limit 10;

update users set age = age + 1 where id = 1;

delete from users where age > 100;
```

Joins are represented in the AST and relational algebra:

```sql
select u.id, o.id
from users u
join orders o on u.id = o.user_id;
```

## Current Limitations

minisqlr is still a learning system. Some important pieces are deliberately
incomplete:

- `ORDER BY` can be optimized into a `Sort` physical operator, but full VM-level
  sort execution is still a TODO.
- Hash join appears as a physical alternative, but code generation currently
  lowers it to nested loops.
- Index metadata and index maintenance are not fully implemented yet.
- Range scans and index-only scans are planned but not wired through the whole
  system.
- Type handling is intentionally simple.
- SQL compatibility is partial.
- Error reporting is practical but not polished.
- The CLI is small and meant for development, not production use.

## TODO

Near-term:

- Add VM sort support for `ORDER BY`.
- Add `IndexScan` and `IndexRangeScan` physical operators.
- Add index schema metadata and `CREATE INDEX`.
- Maintain secondary indexes during `INSERT`, `UPDATE`, and `DELETE`.
- Add rowid lookup and index-to-table lookup code generation.
- Improve `LIMIT` so it can stop scans early when no sort is needed.
- Add better output formatting in the CLI.
- Add statement-level read/write classification for transactions.

Query engine:

- Add predicate pushdown rules.
- Add projection pruning.
- Add join reordering beyond simple commutation.
- Add selectivity estimation from table/index statistics.
- Add covering index detection.
- Support sorting by expressions that are not projected.
- Support more scalar functions.

Storage:

- Add explicit range cursor operations such as `SeekGe`, `SeekGt`, and `IdxGt`.
- Improve B-tree cursor behavior after mutation during scans.
- Add page cache instrumentation.
- Strengthen journal recovery tests.
- Add integrity checking tools.

SQL surface:

- Add `CREATE INDEX`.
- Add `ALTER TABLE` basics.
- Add `EXPLAIN`.
- Add `GROUP BY` and simple aggregates.
- Add `IS NULL`, `IN`, `LIKE`, and `BETWEEN`.
- Add prepared statements and parameters.

Developer experience:

- Add golden tests for query plans and VM bytecode.
- Add CLI integration tests.
- Add documentation diagrams for the parser, optimizer, VM, and storage layers.
- Keep modules small and readable for teaching.

## Why This Project Exists

Many toy databases stop at either a parser or a key-value store. minisqlr tries
to connect the whole path:

```text
SQL -> optimizer -> VM -> storage
```

That makes it useful for learning how real relational databases are assembled,
without needing to digest a production-scale codebase first.

The project favors clarity over completeness. When a design choice is made, it
should be easy to point at the corresponding file and understand the idea.
