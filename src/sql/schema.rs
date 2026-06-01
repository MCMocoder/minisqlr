use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::engine::btree::{BTree, BTreeCursor, BTreeDataType, BTreeValueType};

// ── Schema type tag ─────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SchemaType {
    #[serde(rename = "table")]
    Table,
    #[serde(rename = "index")]
    Index,
    #[serde(rename = "view")]
    View,
    #[serde(rename = "trigger")]
    Trigger,
}

impl SchemaType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SchemaType::Table => "table",
            SchemaType::Index => "index",
            SchemaType::View => "view",
            SchemaType::Trigger => "trigger",
        }
    }

    pub fn from_str(s: &str) -> Option<SchemaType> {
        match s {
            "table" => Some(SchemaType::Table),
            "index" => Some(SchemaType::Index),
            "view" => Some(SchemaType::View),
            "trigger" => Some(SchemaType::Trigger),
            _ => None,
        }
    }
}

// ── Column definition (stored as JSON in structure column) ─────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub col_type: String,
    pub nullable: bool,
    pub is_primary_key: bool,
}

/// The JSON payload stored in sqlite_schema.structure for a table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableStructure {
    pub columns: Vec<ColumnDef>,
}

// ── Deserialised schema row ─────────────────────────────────────

#[derive(Clone, Debug)]
pub struct SchemaInfo {
    pub schema_type: SchemaType,
    pub name: String,
    pub tbl_name: String,
    pub root_pgno: u32,
    pub structure_json: String,
}

// ── Schema manager ──────────────────────────────────────────────

/// Manages the built-in hidden `sqlite_schema` table rooted at page 1.
///
/// Row layout (key = table name):
///   type TEXT  |  name TEXT  |  tbl_name TEXT  |  rootpage INTEGER  |  structure TEXT (JSON)
pub struct SchemaManager {
    btree: Rc<BTree>,
}

impl SchemaManager {
    pub fn new(btree: Rc<BTree>) -> Self {
        SchemaManager { btree }
    }

    /// Open a fresh cursor on the schema table (page 1).
    fn new_cursor(&self) -> BTreeCursor {
        BTreeCursor::new_root(&self.btree)
    }

    // ── CRUD ────────────────────────────────────────────────────

    /// Insert a table row into sqlite_schema.
    pub fn create_table(&self, name: &str, root_pgno: u32, structure: &TableStructure) -> bool {
        let mut cursor = self.new_cursor();
        let key = BTreeDataType::Text(name.to_string());
        let json = serde_json::to_string(structure).unwrap();
        let value = BTreeValueType::from_vec(vec![
            BTreeDataType::Text(SchemaType::Table.as_str().to_string()),
            BTreeDataType::Text(name.to_string()),
            BTreeDataType::Text(name.to_string()),
            BTreeDataType::Integer(root_pgno as u64),
            BTreeDataType::Text(json),
        ]);
        cursor.insert(&key, &value)
    }

    /// Look up a schema row by table name.
    pub fn get_table(&self, name: &str) -> Option<SchemaInfo> {
        let mut cursor = self.new_cursor();
        let key = BTreeDataType::Text(name.to_string());
        let value = cursor.get_val(&key)?;
        Self::row_to_info(&value)
    }

    /// Remove a table row from sqlite_schema.
    pub fn drop_table(&self, name: &str) -> bool {
        let mut cursor = self.new_cursor();
        let key = BTreeDataType::Text(name.to_string());
        cursor.remove(&key)
    }

    /// Check whether a table name exists in the schema.
    pub fn table_exists(&self, name: &str) -> bool {
        let mut cursor = self.new_cursor();
        let key = BTreeDataType::Text(name.to_string());
        cursor.get_val(&key).is_some()
    }

    /// Return all schema rows stored in sqlite_schema.
    pub fn list_schema(&self) -> Vec<SchemaInfo> {
        let mut cursor = self.new_cursor();
        let mut rows = Vec::new();

        if !cursor.moveto_first_entry() {
            return rows;
        }

        loop {
            if let Some((_, value)) = cursor.current_entry() {
                if let Some(info) = Self::row_to_info(&value) {
                    rows.push(info);
                }
            }

            if !cursor.moveto_next_entry() {
                break;
            }
        }

        rows
    }

    /// Return user table names.
    pub fn table_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .list_schema()
            .into_iter()
            .filter(|info| matches!(info.schema_type, SchemaType::Table))
            .map(|info| info.name)
            .collect();
        names.sort();
        names
    }

    // ── Row decoding ────────────────────────────────────────────

    fn row_to_info(value: &BTreeValueType) -> Option<SchemaInfo> {
        if value.v.len() < 5 {
            return None;
        }

        let schema_type = match &value.v[0] {
            BTreeDataType::Text(s) => SchemaType::from_str(s)?,
            _ => return None,
        };

        let name = match &value.v[1] {
            BTreeDataType::Text(s) => s.clone(),
            _ => return None,
        };

        let tbl_name = match &value.v[2] {
            BTreeDataType::Text(s) => s.clone(),
            _ => return None,
        };

        let root_pgno = match value.v[3] {
            BTreeDataType::Integer(n) => n as u32,
            _ => return None,
        };

        let structure_json = match &value.v[4] {
            BTreeDataType::Text(s) => s.clone(),
            _ => return None,
        };

        Some(SchemaInfo {
            schema_type,
            name,
            tbl_name,
            root_pgno,
            structure_json,
        })
    }

    /// Parse the structure_json column back into a TableStructure.
    pub fn parse_structure(info: &SchemaInfo) -> Option<TableStructure> {
        serde_json::from_str(&info.structure_json).ok()
    }
}
