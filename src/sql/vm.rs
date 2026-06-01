use std::cell::RefCell;
use std::rc::Rc;

use crate::engine::btree::{BTree, BTreeCursor, BTreeDataType};
use crate::sql::operations::VMOperation;
use crate::sql::schema::SchemaManager;

pub struct VMInstance {
    pub btree: Rc<BTree>,
    pub schema: RefCell<SchemaManager>,
    pub cursors: RefCell<Vec<Option<BTreeCursor>>>,
    pub registers: RefCell<Vec<BTreeDataType>>,
    pub pc: RefCell<usize>,
    pub flag_halt: RefCell<bool>,
    pub flag_cursor_end: RefCell<bool>,
    pub results: RefCell<Vec<Vec<BTreeDataType>>>,
}

impl VMInstance {
    pub fn new(btree: Rc<BTree>) -> Self {
        Self {
            schema: RefCell::new(SchemaManager::new(btree.clone())),
            btree,
            cursors: RefCell::new(Vec::new()),
            registers: RefCell::new(Vec::new()),
            pc: RefCell::new(0),
            flag_halt: RefCell::new(false),
            flag_cursor_end: RefCell::new(false),
            results: RefCell::new(Vec::new()),
        }
    }

    pub fn reset(&self) {
        self.cursors.borrow_mut().clear();
        self.registers.borrow_mut().clear();
        *self.pc.borrow_mut() = 0;
        *self.flag_halt.borrow_mut() = false;
        *self.flag_cursor_end.borrow_mut() = false;
        self.results.borrow_mut().clear();
    }

    pub fn run(&self, ops: &[Box<dyn VMOperation>]) -> Result<(), VMError> {
        *self.pc.borrow_mut() = 0;
        *self.flag_halt.borrow_mut() = false;

        loop {
            if *self.flag_halt.borrow() {
                return Ok(());
            }

            let pc = *self.pc.borrow();
            let Some(op) = ops.get(pc) else {
                return Ok(());
            };

            let explain = op.explain();
            if !op.execute(self) {
                return Err(VMError {
                    message: format!("operation failed at pc={pc}: {explain}"),
                });
            }

            if *self.pc.borrow() == pc {
                *self.pc.borrow_mut() = pc + 1;
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VMError {
    pub message: String,
}

impl std::fmt::Display for VMError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vm error: {}", self.message)
    }
}

impl std::error::Error for VMError {}
