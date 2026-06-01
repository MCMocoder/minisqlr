use std::cmp::Ordering;

use crate::engine::btree::{BTreeCursor, BTreeDataType, BTreeValueType};
use crate::sql::schema;
use crate::sql::vm::VMInstance;

pub trait VMOperation {
    fn execute(&self, vm: &VMInstance) -> bool;

    /// Human-readable description of this operation (for EXPLAIN).
    fn explain(&self) -> String;
}

fn read_register(vm: &VMInstance, reg: usize) -> Option<BTreeDataType> {
    vm.registers.borrow().get(reg).cloned()
}

fn write_register(vm: &VMInstance, reg: usize, value: BTreeDataType) {
    let mut registers = vm.registers.borrow_mut();
    if registers.len() <= reg {
        registers.resize_with(reg + 1, || BTreeDataType::Null);
    }
    registers[reg] = value;
}

fn bool_value(value: bool) -> BTreeDataType {
    BTreeDataType::Integer(if value { 1 } else { 0 })
}

fn is_truthy(value: &BTreeDataType) -> bool {
    match value {
        BTreeDataType::Null => false,
        BTreeDataType::Integer(value) => *value != 0,
        BTreeDataType::Real(value) => *value != 0.0,
        BTreeDataType::Text(value) => !value.is_empty(),
        BTreeDataType::Blob(value) => !value.is_empty(),
    }
}

fn record_from_register(value: BTreeDataType) -> BTreeValueType {
    match value {
        BTreeDataType::Blob(bytes) => BTreeValueType::new(&bytes),
        scalar => BTreeValueType::new_single(scalar),
    }
}

fn type_rank(value: &BTreeDataType) -> u8 {
    match value {
        BTreeDataType::Null => 0,
        BTreeDataType::Integer(_) | BTreeDataType::Real(_) => 1,
        BTreeDataType::Text(_) => 2,
        BTreeDataType::Blob(_) => 3,
    }
}

fn compare_values(lhs: &BTreeDataType, rhs: &BTreeDataType) -> Option<Ordering> {
    match (lhs, rhs) {
        (BTreeDataType::Null, BTreeDataType::Null) => Some(Ordering::Equal),
        (BTreeDataType::Integer(lhs), BTreeDataType::Integer(rhs)) => Some(lhs.cmp(rhs)),
        (BTreeDataType::Real(lhs), BTreeDataType::Real(rhs)) => lhs.partial_cmp(rhs),
        (BTreeDataType::Integer(lhs), BTreeDataType::Real(rhs)) => (*lhs as f64).partial_cmp(rhs),
        (BTreeDataType::Real(lhs), BTreeDataType::Integer(rhs)) => lhs.partial_cmp(&(*rhs as f64)),
        (BTreeDataType::Text(lhs), BTreeDataType::Text(rhs)) => Some(lhs.cmp(rhs)),
        (BTreeDataType::Blob(lhs), BTreeDataType::Blob(rhs)) => Some(lhs.cmp(rhs)),
        _ => Some(type_rank(lhs).cmp(&type_rank(rhs))),
    }
}

fn compare_into_register(
    vm: &VMInstance,
    lhs: usize,
    rhs: usize,
    dest: usize,
    predicate: impl FnOnce(Ordering) -> bool,
) -> bool {
    let lhs = match read_register(vm, lhs) {
        Some(value) => value,
        None => return false,
    };
    let rhs = match read_register(vm, rhs) {
        Some(value) => value,
        None => return false,
    };
    let result = compare_values(&lhs, &rhs).map(predicate).unwrap_or(false);
    write_register(vm, dest, bool_value(result));
    true
}

fn numeric_value(value: &BTreeDataType) -> Option<f64> {
    match value {
        BTreeDataType::Integer(value) => Some(*value as f64),
        BTreeDataType::Real(value) => Some(*value),
        _ => None,
    }
}

fn numeric_binary_into_register(
    vm: &VMInstance,
    lhs: usize,
    rhs: usize,
    dest: usize,
    op: impl FnOnce(f64, f64) -> f64,
) -> bool {
    let lhs = match read_register(vm, lhs).and_then(|value| numeric_value(&value)) {
        Some(value) => value,
        None => return false,
    };
    let rhs = match read_register(vm, rhs).and_then(|value| numeric_value(&value)) {
        Some(value) => value,
        None => return false,
    };
    let result = op(lhs, rhs);
    if result.is_finite() && result.fract() == 0.0 && result >= 0.0 && result <= u64::MAX as f64 {
        write_register(vm, dest, BTreeDataType::Integer(result as u64));
    } else {
        write_register(vm, dest, BTreeDataType::Real(result));
    }
    true
}

// -- Transaction ------------------------------------------------

pub struct BeginTransaction;
pub struct CommitTransaction;
pub struct RollbackTransaction;

impl VMOperation for BeginTransaction {
    fn execute(&self, vm: &VMInstance) -> bool {
        vm.btree.begin_transaction_write();
        true
    }

    fn explain(&self) -> String {
        "BeginTransaction".into()
    }
}

impl VMOperation for CommitTransaction {
    fn execute(&self, vm: &VMInstance) -> bool {
        vm.btree.end_transaction_write();
        true
    }

    fn explain(&self) -> String {
        "CommitTransaction".into()
    }
}

impl VMOperation for RollbackTransaction {
    fn execute(&self, vm: &VMInstance) -> bool {
        vm.btree.rollback();
        true
    }

    fn explain(&self) -> String {
        "RollbackTransaction".into()
    }
}

// -- Schema -----------------------------------------------------

pub struct CreateTable {
    pub name: String,
    pub columns: Vec<schema::ColumnDef>,
}

impl VMOperation for CreateTable {
    fn execute(&self, vm: &VMInstance) -> bool {
        // 1. Allocate a new leaf page for the table data
        let node = vm.btree.newnode(true, true);
        let root_pgno = node.pgno();
        // Persist the empty page header to disk
        let mut node = node;
        node.write_buf();
        drop(node);

        // 2. Wrap columns in a TableStructure
        let structure = schema::TableStructure {
            columns: self.columns.clone(),
        };

        // 3. Write the schema row
        let mgr = vm.schema.borrow();
        mgr.create_table(&self.name, root_pgno, &structure)
    }

    fn explain(&self) -> String {
        let cols: Vec<String> = self
            .columns
            .iter()
            .map(|c| format!("{} {}", c.name, c.col_type))
            .collect();
        format!("CreateTable {} ({})", self.name, cols.join(", "))
    }
}

pub struct DropTable {
    pub name: String,
}

impl VMOperation for DropTable {
    fn execute(&self, vm: &VMInstance) -> bool {
        let mgr = vm.schema.borrow();

        // 1. Look up the table to get its root page number
        let info = match mgr.get_table(&self.name) {
            Some(i) => i,
            None => return false,
        };

        let root_pgno = info.root_pgno;

        // 2. Remove the schema row
        if !mgr.drop_table(&self.name) {
            return false;
        }
        drop(mgr);

        // 3. Recycle the root page into the freelist
        vm.btree.free_tree(root_pgno);

        true
    }

    fn explain(&self) -> String {
        format!("DropTable {}", self.name)
    }
}

// -- Cursor management -----------------------------------------

pub struct OpenWrite {
    pub cursor_id: usize,
    pub root_pgno: u32,
}

pub struct OpenRead {
    pub cursor_id: usize,
    pub root_pgno: u32,
}

pub struct Close {
    pub cursor_id: usize,
}

impl VMOperation for OpenWrite {
    fn execute(&self, vm: &VMInstance) -> bool {
        let mut cursors = vm.cursors.borrow_mut();
        if cursors.len() <= self.cursor_id {
            cursors.resize_with(self.cursor_id + 1, || None);
        }
        cursors[self.cursor_id] = Some(BTreeCursor::new(
            &vm.btree,
            self.cursor_id as u32,
            self.root_pgno,
        ));
        true
    }

    fn explain(&self) -> String {
        format!(
            "OpenWrite cursor={} root={}",
            self.cursor_id, self.root_pgno
        )
    }
}

impl VMOperation for OpenRead {
    fn execute(&self, vm: &VMInstance) -> bool {
        let mut cursors = vm.cursors.borrow_mut();
        if cursors.len() <= self.cursor_id {
            cursors.resize_with(self.cursor_id + 1, || None);
        }
        cursors[self.cursor_id] = Some(BTreeCursor::new(
            &vm.btree,
            self.cursor_id as u32,
            self.root_pgno,
        ));
        true
    }

    fn explain(&self) -> String {
        format!("OpenRead cursor={} root={}", self.cursor_id, self.root_pgno)
    }
}

impl VMOperation for Close {
    fn execute(&self, vm: &VMInstance) -> bool {
        let mut cursors = vm.cursors.borrow_mut();
        match cursors.get_mut(self.cursor_id) {
            Some(cursor) => {
                let was_open = cursor.is_some();
                *cursor = None;
                was_open
            }
            None => false,
        }
    }

    fn explain(&self) -> String {
        format!("Close cursor={}", self.cursor_id)
    }
}

// -- Cursor movement -------------------------------------------

pub struct Rewind {
    pub cursor_id: usize,
}

pub struct MoveNext {
    pub cursor_id: usize,
}

pub struct SeekRowid {
    pub cursor_id: usize,
    pub key_reg: usize,
}

impl VMOperation for Rewind {
    fn execute(&self, vm: &VMInstance) -> bool {
        let mut cursors = vm.cursors.borrow_mut();
        let cursor = match cursors.get_mut(self.cursor_id).and_then(Option::as_mut) {
            Some(cursor) => cursor,
            None => return false,
        };

        let found = cursor.moveto_first_entry();
        *vm.flag_cursor_end.borrow_mut() = !found;
        true
    }

    fn explain(&self) -> String {
        format!("Rewind cursor={}", self.cursor_id)
    }
}

impl VMOperation for MoveNext {
    fn execute(&self, vm: &VMInstance) -> bool {
        let mut cursors = vm.cursors.borrow_mut();
        let cursor = match cursors.get_mut(self.cursor_id).and_then(Option::as_mut) {
            Some(cursor) => cursor,
            None => return false,
        };

        let found = cursor.moveto_next_entry();
        *vm.flag_cursor_end.borrow_mut() = !found;
        true
    }

    fn explain(&self) -> String {
        format!("MoveNext cursor={}", self.cursor_id)
    }
}

impl VMOperation for SeekRowid {
    fn execute(&self, vm: &VMInstance) -> bool {
        let key = match vm.registers.borrow().get(self.key_reg) {
            Some(BTreeDataType::Integer(rowid)) => BTreeDataType::Integer(*rowid),
            Some(_) => return false,
            None => return false,
        };

        let mut cursors = vm.cursors.borrow_mut();
        let cursor = match cursors.get_mut(self.cursor_id).and_then(Option::as_mut) {
            Some(cursor) => cursor,
            None => return false,
        };

        let found = cursor.moveto_key_entry(&key);
        *vm.flag_cursor_end.borrow_mut() = !found;
        true
    }

    fn explain(&self) -> String {
        format!(
            "SeekRowid cursor={} key_reg={}",
            self.cursor_id, self.key_reg
        )
    }
}

// -- Register operations ---------------------------------------

pub struct Integer {
    pub dest: usize,
    pub value: i64,
}

pub struct Real {
    pub dest: usize,
    pub value: f64,
}

pub struct Text {
    pub dest: usize,
    pub value: Vec<u8>,
}

pub struct Null {
    pub dest: usize,
}

pub struct MakeRecord {
    pub dest: usize,
    pub fields: Vec<usize>,
}

pub struct Column {
    pub cursor_id: usize,
    pub col_idx: usize,
    pub dest: usize,
}

pub struct Rowid {
    pub cursor_id: usize,
    pub dest: usize,
}

impl VMOperation for Integer {
    fn execute(&self, vm: &VMInstance) -> bool {
        let value = match u64::try_from(self.value) {
            Ok(value) => value,
            Err(_) => return false,
        };
        write_register(vm, self.dest, BTreeDataType::Integer(value));
        true
    }

    fn explain(&self) -> String {
        format!("Integer {} -> r{}", self.value, self.dest)
    }
}

impl VMOperation for Real {
    fn execute(&self, vm: &VMInstance) -> bool {
        write_register(vm, self.dest, BTreeDataType::Real(self.value));
        true
    }

    fn explain(&self) -> String {
        format!("Real {} -> r{}", self.value, self.dest)
    }
}

impl VMOperation for Text {
    fn execute(&self, vm: &VMInstance) -> bool {
        let value = String::from_utf8_lossy(&self.value).into_owned();
        write_register(vm, self.dest, BTreeDataType::Text(value));
        true
    }

    fn explain(&self) -> String {
        format!("Text len={} -> r{}", self.value.len(), self.dest)
    }
}

impl VMOperation for Null {
    fn execute(&self, vm: &VMInstance) -> bool {
        write_register(vm, self.dest, BTreeDataType::Null);
        true
    }

    fn explain(&self) -> String {
        format!("Null -> r{}", self.dest)
    }
}

impl VMOperation for MakeRecord {
    fn execute(&self, vm: &VMInstance) -> bool {
        let registers = vm.registers.borrow();
        let mut fields = Vec::with_capacity(self.fields.len());
        for reg in &self.fields {
            let value = match registers.get(*reg) {
                Some(value) => value.clone(),
                None => return false,
            };
            fields.push(value);
        }
        drop(registers);

        let record = BTreeValueType::from_vec(fields);
        write_register(vm, self.dest, BTreeDataType::Blob(record.get_bytes()));
        true
    }

    fn explain(&self) -> String {
        let fields: Vec<String> = self.fields.iter().map(|reg| format!("r{reg}")).collect();
        format!("MakeRecord [{}] -> r{}", fields.join(", "), self.dest)
    }
}

impl VMOperation for Column {
    fn execute(&self, vm: &VMInstance) -> bool {
        let value = {
            let cursors = vm.cursors.borrow();
            let cursor = match cursors.get(self.cursor_id).and_then(Option::as_ref) {
                Some(cursor) => cursor,
                None => return false,
            };
            let (_, record) = match cursor.current_entry() {
                Some(entry) => entry,
                None => return false,
            };
            record
                .v
                .get(self.col_idx)
                .cloned()
                .unwrap_or(BTreeDataType::Null)
        };

        write_register(vm, self.dest, value);
        true
    }

    fn explain(&self) -> String {
        format!(
            "Column cursor={} col={} -> r{}",
            self.cursor_id, self.col_idx, self.dest
        )
    }
}

impl VMOperation for Rowid {
    fn execute(&self, vm: &VMInstance) -> bool {
        let key = {
            let cursors = vm.cursors.borrow();
            let cursor = match cursors.get(self.cursor_id).and_then(Option::as_ref) {
                Some(cursor) => cursor,
                None => return false,
            };
            let (key, _) = match cursor.current_entry() {
                Some(entry) => entry,
                None => return false,
            };
            key
        };

        write_register(vm, self.dest, key);
        true
    }

    fn explain(&self) -> String {
        format!("Rowid cursor={} -> r{}", self.cursor_id, self.dest)
    }
}

// -- Row mutation ----------------------------------------------

pub struct NewRowid {
    pub cursor_id: usize,
    pub dest: usize,
}

pub struct InsertRow {
    pub cursor_id: usize,
    pub key_reg: usize,
    pub value_reg: usize,
}

pub struct DeleteRow {
    pub cursor_id: usize,
    pub key_reg: usize,
}

pub struct UpdateRow {
    pub cursor_id: usize,
    pub key_reg: usize,
    pub value_reg: usize,
}

impl VMOperation for NewRowid {
    fn execute(&self, vm: &VMInstance) -> bool {
        let rowid = {
            let mut cursors = vm.cursors.borrow_mut();
            let cursor = match cursors.get_mut(self.cursor_id).and_then(Option::as_mut) {
                Some(cursor) => cursor,
                None => return false,
            };
            match cursor.next_integer_key() {
                Some(rowid) => rowid,
                None => return false,
            }
        };

        write_register(vm, self.dest, BTreeDataType::Integer(rowid));
        true
    }

    fn explain(&self) -> String {
        format!("NewRowid cursor={} -> r{}", self.cursor_id, self.dest)
    }
}

impl VMOperation for InsertRow {
    fn execute(&self, vm: &VMInstance) -> bool {
        let key = match read_register(vm, self.key_reg) {
            Some(key) => key,
            None => return false,
        };
        let value = match read_register(vm, self.value_reg) {
            Some(value) => record_from_register(value),
            None => return false,
        };

        let mut cursors = vm.cursors.borrow_mut();
        let cursor = match cursors.get_mut(self.cursor_id).and_then(Option::as_mut) {
            Some(cursor) => cursor,
            None => return false,
        };
        cursor.insert(&key, &value)
    }

    fn explain(&self) -> String {
        format!(
            "InsertRow cursor={} key=r{} value=r{}",
            self.cursor_id, self.key_reg, self.value_reg
        )
    }
}

impl VMOperation for DeleteRow {
    fn execute(&self, vm: &VMInstance) -> bool {
        let key = match read_register(vm, self.key_reg) {
            Some(key) => key,
            None => return false,
        };

        let mut cursors = vm.cursors.borrow_mut();
        let cursor = match cursors.get_mut(self.cursor_id).and_then(Option::as_mut) {
            Some(cursor) => cursor,
            None => return false,
        };
        cursor.remove(&key)
    }

    fn explain(&self) -> String {
        format!("DeleteRow cursor={} key=r{}", self.cursor_id, self.key_reg)
    }
}

impl VMOperation for UpdateRow {
    fn execute(&self, vm: &VMInstance) -> bool {
        let key = match read_register(vm, self.key_reg) {
            Some(key) => key,
            None => return false,
        };
        let value = match read_register(vm, self.value_reg) {
            Some(value) => record_from_register(value),
            None => return false,
        };

        let mut cursors = vm.cursors.borrow_mut();
        let cursor = match cursors.get_mut(self.cursor_id).and_then(Option::as_mut) {
            Some(cursor) => cursor,
            None => return false,
        };
        if !cursor.remove(&key) {
            return false;
        }
        cursor.insert(&key, &value)
    }

    fn explain(&self) -> String {
        format!(
            "UpdateRow cursor={} key=r{} value=r{}",
            self.cursor_id, self.key_reg, self.value_reg
        )
    }
}

// -- Comparison ------------------------------------------------

pub struct Eq {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Ne {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Lt {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Le {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Gt {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Ge {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct And {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Or {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Not {
    pub src: usize,
    pub dest: usize,
}
pub struct Negate {
    pub src: usize,
    pub dest: usize,
}
pub struct Add {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Subtract {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Multiply {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}
pub struct Divide {
    pub lhs: usize,
    pub rhs: usize,
    pub dest: usize,
}

impl VMOperation for Eq {
    fn execute(&self, vm: &VMInstance) -> bool {
        compare_into_register(vm, self.lhs, self.rhs, self.dest, |ord| {
            ord == Ordering::Equal
        })
    }

    fn explain(&self) -> String {
        format!("Eq r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Ne {
    fn execute(&self, vm: &VMInstance) -> bool {
        compare_into_register(vm, self.lhs, self.rhs, self.dest, |ord| {
            ord != Ordering::Equal
        })
    }

    fn explain(&self) -> String {
        format!("Ne r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Lt {
    fn execute(&self, vm: &VMInstance) -> bool {
        compare_into_register(vm, self.lhs, self.rhs, self.dest, |ord| {
            ord == Ordering::Less
        })
    }

    fn explain(&self) -> String {
        format!("Lt r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Le {
    fn execute(&self, vm: &VMInstance) -> bool {
        compare_into_register(vm, self.lhs, self.rhs, self.dest, |ord| {
            ord != Ordering::Greater
        })
    }

    fn explain(&self) -> String {
        format!("Le r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Gt {
    fn execute(&self, vm: &VMInstance) -> bool {
        compare_into_register(vm, self.lhs, self.rhs, self.dest, |ord| {
            ord == Ordering::Greater
        })
    }

    fn explain(&self) -> String {
        format!("Gt r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Ge {
    fn execute(&self, vm: &VMInstance) -> bool {
        compare_into_register(vm, self.lhs, self.rhs, self.dest, |ord| {
            ord != Ordering::Less
        })
    }

    fn explain(&self) -> String {
        format!("Ge r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for And {
    fn execute(&self, vm: &VMInstance) -> bool {
        let lhs = match read_register(vm, self.lhs) {
            Some(value) => value,
            None => return false,
        };
        let rhs = match read_register(vm, self.rhs) {
            Some(value) => value,
            None => return false,
        };
        write_register(
            vm,
            self.dest,
            bool_value(is_truthy(&lhs) && is_truthy(&rhs)),
        );
        true
    }

    fn explain(&self) -> String {
        format!("And r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Or {
    fn execute(&self, vm: &VMInstance) -> bool {
        let lhs = match read_register(vm, self.lhs) {
            Some(value) => value,
            None => return false,
        };
        let rhs = match read_register(vm, self.rhs) {
            Some(value) => value,
            None => return false,
        };
        write_register(
            vm,
            self.dest,
            bool_value(is_truthy(&lhs) || is_truthy(&rhs)),
        );
        true
    }

    fn explain(&self) -> String {
        format!("Or r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Not {
    fn execute(&self, vm: &VMInstance) -> bool {
        let value = match read_register(vm, self.src) {
            Some(value) => value,
            None => return false,
        };
        write_register(vm, self.dest, bool_value(!is_truthy(&value)));
        true
    }

    fn explain(&self) -> String {
        format!("Not r{} -> r{}", self.src, self.dest)
    }
}

impl VMOperation for Negate {
    fn execute(&self, vm: &VMInstance) -> bool {
        let value = match read_register(vm, self.src) {
            Some(value) => value,
            None => return false,
        };
        let negated = match value {
            BTreeDataType::Integer(value) => BTreeDataType::Real(-(value as f64)),
            BTreeDataType::Real(value) => BTreeDataType::Real(-value),
            _ => return false,
        };
        write_register(vm, self.dest, negated);
        true
    }

    fn explain(&self) -> String {
        format!("Negate r{} -> r{}", self.src, self.dest)
    }
}

impl VMOperation for Add {
    fn execute(&self, vm: &VMInstance) -> bool {
        numeric_binary_into_register(vm, self.lhs, self.rhs, self.dest, |lhs, rhs| lhs + rhs)
    }

    fn explain(&self) -> String {
        format!("Add r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Subtract {
    fn execute(&self, vm: &VMInstance) -> bool {
        numeric_binary_into_register(vm, self.lhs, self.rhs, self.dest, |lhs, rhs| lhs - rhs)
    }

    fn explain(&self) -> String {
        format!("Subtract r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Multiply {
    fn execute(&self, vm: &VMInstance) -> bool {
        numeric_binary_into_register(vm, self.lhs, self.rhs, self.dest, |lhs, rhs| lhs * rhs)
    }

    fn explain(&self) -> String {
        format!("Multiply r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

impl VMOperation for Divide {
    fn execute(&self, vm: &VMInstance) -> bool {
        let rhs = match read_register(vm, self.rhs) {
            Some(value) => value,
            None => return false,
        };
        if numeric_value(&rhs) == Some(0.0) {
            write_register(vm, self.dest, BTreeDataType::Null);
            return true;
        }
        let lhs = match read_register(vm, self.lhs) {
            Some(value) => value,
            None => return false,
        };
        let lhs = match numeric_value(&lhs) {
            Some(value) => value,
            None => return false,
        };
        let rhs = numeric_value(&rhs).expect("checked above");
        write_register(vm, self.dest, BTreeDataType::Real(lhs / rhs));
        true
    }

    fn explain(&self) -> String {
        format!("Divide r{} r{} -> r{}", self.lhs, self.rhs, self.dest)
    }
}

// -- Control flow ----------------------------------------------

pub struct IfNot {
    pub cond_reg: usize,
    pub target: usize,
}

pub struct IfCursorEnd {
    pub target: usize,
}

pub struct IfResultsGe {
    pub limit: usize,
    pub target: usize,
}

pub struct Goto {
    pub target: usize,
}

impl VMOperation for IfNot {
    fn execute(&self, vm: &VMInstance) -> bool {
        let value = match read_register(vm, self.cond_reg) {
            Some(value) => value,
            None => return false,
        };
        if !is_truthy(&value) {
            *vm.pc.borrow_mut() = self.target;
        }
        true
    }

    fn explain(&self) -> String {
        format!("IfNot r{} -> pc={}", self.cond_reg, self.target)
    }
}

impl VMOperation for IfCursorEnd {
    fn execute(&self, vm: &VMInstance) -> bool {
        if *vm.flag_cursor_end.borrow() {
            *vm.pc.borrow_mut() = self.target;
        }
        true
    }

    fn explain(&self) -> String {
        format!("IfCursorEnd -> pc={}", self.target)
    }
}

impl VMOperation for IfResultsGe {
    fn execute(&self, vm: &VMInstance) -> bool {
        if vm.results.borrow().len() >= self.limit {
            *vm.pc.borrow_mut() = self.target;
        }
        true
    }

    fn explain(&self) -> String {
        format!("IfResultsGe {} -> pc={}", self.limit, self.target)
    }
}

impl VMOperation for Goto {
    fn execute(&self, vm: &VMInstance) -> bool {
        *vm.pc.borrow_mut() = self.target;
        true
    }

    fn explain(&self) -> String {
        format!("Goto pc={}", self.target)
    }
}

// -- Output ----------------------------------------------------

pub struct ResultRow {
    pub fields: Vec<usize>,
}

pub struct Halt;

impl VMOperation for ResultRow {
    fn execute(&self, vm: &VMInstance) -> bool {
        let registers = vm.registers.borrow();
        let mut row = Vec::with_capacity(self.fields.len());
        for reg in &self.fields {
            let value = match registers.get(*reg) {
                Some(value) => value.clone(),
                None => return false,
            };
            row.push(value);
        }
        drop(registers);

        vm.results.borrow_mut().push(row);
        true
    }

    fn explain(&self) -> String {
        let fields: Vec<String> = self.fields.iter().map(|reg| format!("r{reg}")).collect();
        format!("ResultRow [{}]", fields.join(", "))
    }
}

impl VMOperation for Halt {
    fn execute(&self, vm: &VMInstance) -> bool {
        *vm.flag_halt.borrow_mut() = true;
        true
    }

    fn explain(&self) -> String {
        "Halt".into()
    }
}
