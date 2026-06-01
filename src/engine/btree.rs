use core::panic;
use std::{
    borrow::Borrow,
    cell::{Cell, RefCell},
    cmp::Ordering,
    collections::VecDeque,
    fmt::Display,
    mem::size_of,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use super::pages::{PagePtr, Pager, PAGE_EMPTY, PAGE_SIZE};

pub enum BTreeDataType {
    Null,
    Integer(u64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Clone for BTreeDataType {
    fn clone(&self) -> Self {
        match self {
            Self::Null => Self::Null,
            Self::Integer(arg0) => Self::Integer(arg0.clone()),
            Self::Real(arg0) => Self::Real(arg0.clone()),
            Self::Text(arg0) => Self::Text(arg0.clone()),
            Self::Blob(arg0) => Self::Blob(arg0.clone()),
        }
    }
}

impl Display for BTreeDataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BTreeDataType::Null => write!(f, "Null"),
            BTreeDataType::Integer(i) => write!(f, "Intg({})", i),
            BTreeDataType::Real(r) => write!(f, "Real({})", r),
            BTreeDataType::Text(t) => write!(f, "Text({})", t),
            BTreeDataType::Blob(v) => {
                write!(f, "Blob([")?;
                for i in v {
                    write!(f, "{:0X},", *i)?;
                }
                Ok(())
            }
        }
    }
}

impl BTreeDataType {
    pub fn size(&self) -> usize {
        match self {
            BTreeDataType::Null => 1,
            BTreeDataType::Integer(_) => size_of::<u64>() + 1,
            BTreeDataType::Real(_) => size_of::<f64>() + 1,
            BTreeDataType::Text(s) => s.bytes().len() + 2 + 1,
            BTreeDataType::Blob(v) => v.len() + 2 + 1,
        }
    }

    pub fn get_bytes(&self) -> Vec<u8> {
        match self {
            BTreeDataType::Null => vec![0u8],
            BTreeDataType::Integer(i) => {
                let mut vec = vec![1u8];
                vec.append(&mut i.to_be_bytes().to_vec());
                vec
            }
            BTreeDataType::Real(f) => {
                let mut vec = vec![2u8];
                vec.append(&mut f.to_be_bytes().to_vec());
                vec
            }
            BTreeDataType::Text(s) => {
                let mut vec = vec![3u8];
                vec.append(&mut (s.len() as u16).to_be_bytes().to_vec());
                vec.append(&mut s.clone().into_bytes());
                vec
            }
            BTreeDataType::Blob(v) => {
                let mut vec = vec![4u8];
                vec.append(&mut (v.len() as u16).to_be_bytes().to_vec());
                vec.append(&mut v.clone());
                vec
            }
        }
    }

    pub fn new(buf: &[u8]) -> Self {
        match buf[0] {
            0u8 => BTreeDataType::Null,
            1u8 => {
                let mut bytes = [0u8; size_of::<u64>()];
                bytes.copy_from_slice(&buf[1..9]);
                BTreeDataType::Integer(u64::from_be_bytes(bytes))
            }
            2u8 => {
                let mut bytes = [0u8; size_of::<f64>()];
                bytes.copy_from_slice(&buf[1..9]);
                BTreeDataType::Real(f64::from_be_bytes(bytes))
            }
            3u8 => {
                let mut len_bytes = [0u8; 2];
                len_bytes.copy_from_slice(&buf[1..3]);
                let len = u16::from_be_bytes(len_bytes);
                BTreeDataType::Text(String::from_utf8_lossy(&buf[3..len as usize + 3]).to_string())
            }
            _ => {
                let mut len_bytes = [0u8; 2];
                len_bytes.copy_from_slice(&buf[1..3]);
                let len = u16::from_be_bytes(len_bytes);
                BTreeDataType::Blob(Vec::from(&buf[3..len as usize + 3]))
            }
        }
    }
}

impl PartialEq for BTreeDataType {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Integer(l0), Self::Integer(r0)) => l0 == r0,
            (Self::Real(l0), Self::Real(r0)) => l0 == r0,
            (Self::Text(l0), Self::Text(r0)) => l0 == r0,
            (Self::Blob(l0), Self::Blob(r0)) => l0 == r0,
            _ => core::mem::discriminant(self) == core::mem::discriminant(other),
        }
    }
}

impl Eq for BTreeDataType {}

impl PartialOrd for BTreeDataType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Null, Self::Null) => Some(Ordering::Equal),
            (Self::Integer(l0), Self::Integer(r0)) => Some(l0.cmp(r0)),
            (Self::Real(l0), Self::Real(r0)) => l0.partial_cmp(r0),
            (Self::Text(l0), Self::Text(r0)) => Some(l0.cmp(r0)),
            (Self::Blob(l0), Self::Blob(r0)) => Some(l0.cmp(r0)),
            _ => None,
        }
    }
}

impl Ord for BTreeDataType {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).expect("Not same datatype")
    }
}

pub struct BTreeValueType {
    pub(crate) v: Vec<BTreeDataType>,
}

impl BTreeValueType {
    pub fn size(&self) -> usize {
        self.v.iter().map(|e| e.size()).sum()
    }

    pub fn get_bytes(&self) -> Vec<u8> {
        let mut res: Vec<u8> = Vec::new();
        for i in &self.v {
            res.append(&mut i.get_bytes());
        }
        res
    }

    pub fn new(buf: &[u8]) -> Self {
        let mut iter = 0;
        let mut v: Vec<BTreeDataType> = Vec::new();
        while iter < buf.len() {
            let newtype = BTreeDataType::new(&buf[iter..]);
            iter = iter + newtype.size();
            v.push(newtype);
        }
        Self { v }
    }

    pub fn new_single(d: BTreeDataType) -> Self {
        Self { v: vec![d] }
    }

    pub fn null() -> Self {
        BTreeValueType { v: Vec::new() }
    }

    pub(crate) fn from_vec(v: Vec<BTreeDataType>) -> Self {
        Self { v }
    }
}

impl Clone for BTreeValueType {
    fn clone(&self) -> Self {
        Self { v: self.v.clone() }
    }
}

impl Display for BTreeValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[")?;
        for i in &self.v {
            write!(f, "{},", i)?;
        }
        write!(f, "]")?;
        Ok(())
    }
}

// 椤甸潰鐨勪竴涓鍥?
pub struct BTreeNode {
    pager: Rc<Pager>,
    pgno: u32,
    is_leaf: bool,
    is_data_table: bool,
    left: u32,
    right: u32,
    children_pgno: Vec<u32>,
    cells: Vec<(BTreeDataType, BTreeValueType)>,
    page: PagePtr,
}

pub struct NodePtr {
    node: Rc<BTreeNode>,
}

impl NodePtr {
    pub fn new(node: BTreeNode) -> Self {
        NodePtr {
            node: Rc::new(node),
        }
    }

    pub fn pgno(&self) -> u32 {
        self.node.pgno
    }
}

impl Drop for NodePtr {
    fn drop(&mut self) {
        self.node.pager.release(&self.node.page);
    }
}

impl Deref for NodePtr {
    type Target = BTreeNode;

    fn deref(&self) -> &Self::Target {
        self.node.borrow()
    }
}

impl DerefMut for NodePtr {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            let ptr = Rc::as_ptr(&self.node) as *mut BTreeNode;
            &mut *ptr
        }
    }
}

impl Clone for NodePtr {
    fn clone(&self) -> Self {
        self.node.pager.acquire(&self.node.page);
        Self {
            node: self.node.clone(),
        }
    }
}

impl BTreeNode {
    const HEADER_ISLEAF: usize = 0; // 1byte
    const HEADER_ISDATA: usize = 1; // 1byte
    const HEADER_CELLLEN: usize = 2; // 2bytes
    const HEADER_RCHILD_PGNO: usize = 4; // 4bytes
    const HEADER_LEFT: usize = 8; // 4bytes
    const HEADER_RIGHT: usize = 12; // 4bytes
    const HEADER_LEN: usize = 16;

    pub(crate) fn is_leaf_node(&self) -> bool {
        self.is_leaf
    }
    pub(crate) fn children_pgno(&self) -> &[u32] {
        &self.children_pgno
    }

    fn parse_cellref_inner(bytes: &[u8]) -> Vec<u16> {
        assert!(bytes.len() % 2 == 0);
        let mut cellref: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let mut n = [0u8; 2];
            n.copy_from_slice(&bytes[i..i + 2]);
            let cell_offset = u16::from_be_bytes(n);
            cellref.push(cell_offset);
            i = i + 2;
        }
        cellref
    }

    fn parse_cellref(data: &[u8], cellref_len: u16) -> Vec<u16> {
        let content = &data[Self::HEADER_LEN..];
        Self::parse_cellref_inner(&content[0..(cellref_len * 2) as usize])
    }

    pub fn load_page(pager: &Rc<Pager>, page: &PagePtr) -> Self {
        let mut p = page.borrow_mut();
        let pgno = p.pgno();
        assert!(p.data_read().len() >= Self::HEADER_LEN);
        let pdata = p.data_read();

        // 瑙ｆ瀽澶撮儴鍏冩暟鎹?
        let isleaf_byte = pdata[Self::HEADER_ISLEAF];
        let isleaf = isleaf_byte != 0;

        let isdata_byte = pdata[Self::HEADER_ISDATA];
        let isdata = isdata_byte != 0;

        let mut reflen_bytes = [0u8; 2];
        reflen_bytes.copy_from_slice(&pdata[Self::HEADER_CELLLEN..Self::HEADER_CELLLEN + 2]);
        let cellref_len = u16::from_be_bytes(reflen_bytes);

        let mut left_bytes = [0u8; 4];
        left_bytes.copy_from_slice(&pdata[Self::HEADER_LEFT..Self::HEADER_LEFT + 4]);
        let left = u32::from_be_bytes(left_bytes);

        let mut right_bytes = [0u8; 4];
        right_bytes.copy_from_slice(&pdata[Self::HEADER_RIGHT..Self::HEADER_RIGHT + 4]);
        let right = u32::from_be_bytes(right_bytes);

        let mut rchild_pgno_bytes = [0u8; 4];
        rchild_pgno_bytes
            .copy_from_slice(&pdata[Self::HEADER_RCHILD_PGNO..Self::HEADER_RCHILD_PGNO + 4]);
        let rchild_pgno = u32::from_be_bytes(rchild_pgno_bytes);

        // 瑙ｆ瀽cellref鏁扮粍
        let cellref = Self::parse_cellref(pdata, cellref_len);

        let mut children_pgno = Vec::<u32>::new();

        let mut cells: Vec<(BTreeDataType, BTreeValueType)> = Vec::new();

        // 鑾峰彇cell鏁版嵁
        for i in cellref {
            let offset = i as usize;
            if !isleaf {
                let mut lchild_bytes = [0u8; 4];
                lchild_bytes.copy_from_slice(&pdata[offset..offset + 4]);
                children_pgno.push(u32::from_be_bytes(lchild_bytes));
            }
            let mut keylen_bytes = [0u8; 4];
            let mut vallen_bytes = [0u8; 4];
            keylen_bytes.copy_from_slice(&pdata[offset + 4..offset + 4 + 4]);
            vallen_bytes.copy_from_slice(&pdata[offset + 4 + 4..offset + 4 + 4 + 4]);
            let keylen = u32::from_be_bytes(keylen_bytes) as usize;
            let vallen = u32::from_be_bytes(vallen_bytes) as usize;
            let buf = &pdata[offset + 12..offset + 12 + keylen + vallen];
            let (kb, vb) = buf.split_at(keylen);
            let key = BTreeDataType::new(kb);
            if !vb.is_empty() {
                let value = BTreeValueType::new(vb);
                cells.push((key.clone(), value.clone()))
            } else {
                cells.push((key.clone(), BTreeValueType::null()));
            }
        }

        if rchild_pgno != 0 {
            children_pgno.push(rchild_pgno);
        }

        Self {
            pager: pager.clone(),
            pgno: pgno,
            is_leaf: isleaf,
            left,
            right,
            is_data_table: isdata,
            page: page.clone(),
            children_pgno: children_pgno,
            cells: cells,
        }
    }

    pub fn load_empty_page(
        pager: &Rc<Pager>,
        page: &PagePtr,
        is_leaf: bool,
        is_data_table: bool,
    ) -> Self {
        let p = page.borrow_mut();
        let pgno = p.pgno();
        let res = Self {
            pager: pager.clone(),
            pgno: pgno,
            is_leaf,
            left: 0,
            right: 0,
            is_data_table,
            page: page.clone(),
            children_pgno: Vec::new(),
            cells: Vec::new(),
        };
        drop(p);
        pager.mark_inited(page);
        res
    }

    pub fn write_buf(&mut self) {
        self.pager.will_modify(&self.page);
        let mut page = self.page.borrow_mut();
        let pdata = page.data_write();
        pdata.copy_from_slice(&[0u8; 4096]);

        if self.is_leaf {
            pdata[0] = 1;
        }
        if self.is_data_table {
            pdata[1] = 1;
        }
        let cell_len_bytes = (self.cells.len() as u16).to_be_bytes();
        pdata[2..4].copy_from_slice(&cell_len_bytes);

        pdata[Self::HEADER_LEFT..Self::HEADER_LEFT + 4]
            .copy_from_slice(&self.left.clone().to_be_bytes());
        pdata[Self::HEADER_RIGHT..Self::HEADER_RIGHT + 4]
            .copy_from_slice(&self.right.clone().to_be_bytes());

        // 鍐欏叆cell鏁版嵁
        let mut cellref_idx = Self::HEADER_LEN;
        let mut child_idx = 0usize;
        let mut prevstart = PAGE_SIZE;
        for i in &self.cells {
            let cell_len = 4 + 4 + 4 + i.0.size() + i.1.size();
            prevstart = prevstart - cell_len;
            if !self.is_leaf {
                pdata[prevstart..prevstart + 4]
                    .copy_from_slice(&self.children_pgno[child_idx].to_be_bytes());
            } else {
                pdata[prevstart..prevstart + 4].copy_from_slice(&0u32.to_be_bytes());
            }
            pdata[prevstart + 4..prevstart + 8].copy_from_slice(&(i.0.size() as u32).to_be_bytes());
            pdata[prevstart + 8..prevstart + 12]
                .copy_from_slice(&(i.1.size() as u32).to_be_bytes());
            pdata[prevstart + 12..prevstart + 12 + i.0.size()].copy_from_slice(&i.0.get_bytes());
            if i.1.size() != 0 {
                pdata[prevstart + 12 + i.0.size()..prevstart + cell_len]
                    .copy_from_slice(&i.1.get_bytes());
            }
            pdata[cellref_idx..cellref_idx + 2].copy_from_slice(&(prevstart as u16).to_be_bytes());
            child_idx = child_idx + 1;
            cellref_idx = cellref_idx + 2;
        }

        if child_idx < self.children_pgno.len() {
            pdata[Self::HEADER_RCHILD_PGNO..Self::HEADER_RCHILD_PGNO + 4]
                .copy_from_slice(&self.children_pgno.last().expect("No").to_be_bytes());
        }
    }

    fn binary_search_cell(&self, key: &BTreeDataType) -> SearchResult {
        if self.cells.is_empty() {
            return SearchResult::Lessthan(0);
        }
        let mut lower = 0usize;
        let mut upper = self.cells.len() - 1;
        loop {
            let i = (lower + upper) / 2;
            let midkey = self.cells[i].0.clone();
            if *key == midkey {
                break SearchResult::Equal(i);
            } else if *key < midkey {
                if i == 0 {
                    break SearchResult::Lessthan(0);
                }
                upper = i - 1;
                if lower > upper {
                    break SearchResult::Lessthan(i);
                }
            } else {
                lower = i + 1;
                if lower >= self.cells.len() {
                    break SearchResult::Right;
                }
                if lower > upper {
                    break SearchResult::Lessthan(i + 1);
                }
            }
        }
    }

    pub fn get_key(&self, key: &BTreeDataType) -> Option<BTreeValueType> {
        let result = self.binary_search_cell(key);
        match result {
            SearchResult::Equal(idx) => Some(self.cells[idx].1.clone()),
            SearchResult::Lessthan(_) => None,
            SearchResult::Right => None,
        }
    }

    pub fn contains_key(&self, key: &BTreeDataType) -> bool {
        if let SearchResult::Equal(_) = self.binary_search_cell(key) {
            true
        } else {
            false
        }
    }

    fn get_key_searchresult(&self, key: &BTreeDataType) -> SearchResult {
        self.binary_search_cell(key)
    }

    pub fn get_minimum_key(&self) -> BTreeDataType {
        self.cells[0].0.clone()
    }

    pub fn get_leftmost_child(&self) -> u32 {
        self.children_pgno[0]
    }

    pub fn get_rightmost_child(&self) -> u32 {
        *self.children_pgno.last().expect("noway")
    }

    pub fn get_leftchild(&self, idx: usize) -> u32 {
        self.children_pgno[idx]
    }

    pub fn get_rightchild(&self, idx: usize) -> u32 {
        self.children_pgno[idx + 1]
    }

    fn insert_leaf(
        &mut self,
        key: &BTreeDataType,
        value: &BTreeValueType,
        m: usize,
    ) -> InsertResult {
        if self.cells.is_empty() {
            self.cells.push((key.clone(), value.clone()));
            self.write_buf();
            return InsertResult::Ok;
        }
        let idx = self.binary_search_cell(key);
        match idx {
            SearchResult::Equal(_) => return InsertResult::Error,
            SearchResult::Lessthan(idx) => self.cells.insert(idx, (key.clone(), value.clone())),
            SearchResult::Right => self.cells.push((key.clone(), value.clone())),
        };
        self.write_buf();
        if self.cells.len() >= 2 * m {
            InsertResult::Needsplit(self.cells[self.cells.len() / 2 - 1].0.clone())
        } else {
            InsertResult::Ok
        }
    }

    fn insert_internal(
        &mut self,
        key: &BTreeDataType,
        lchild: u32,
        rchild: u32,
        m: usize,
    ) -> InsertResult {
        debug_assert!(
            self.cells.len() + 1 == self.children_pgno.len()
                || self.cells.len() == self.children_pgno.len()
                || (self.cells.is_empty() && self.children_pgno.is_empty())
        );

        if self.cells.is_empty() {
            self.cells.push((key.clone(), BTreeValueType::null()));
            self.children_pgno.push(lchild);
            self.children_pgno.push(rchild);
            self.write_buf();
            return InsertResult::Ok;
        }
        let idx = self.binary_search_cell(key);
        match idx {
            SearchResult::Equal(_) => return InsertResult::Error,
            SearchResult::Lessthan(idx) => {
                self.cells
                    .insert(idx, (key.clone(), BTreeValueType::null()));
                self.children_pgno.insert(idx, lchild);
                if idx == self.children_pgno.len() {
                    self.children_pgno.push(rchild);
                } else {
                    self.children_pgno[idx + 1] = rchild;
                }
            }
            SearchResult::Right => {
                self.cells.push((key.clone(), BTreeValueType::null()));
                let last = self.children_pgno.last_mut().expect("noway");
                *last = lchild;
                self.children_pgno.push(rchild);
            }
        };
        self.write_buf();
        if self.cells.len() >= 2 * m {
            InsertResult::Needsplit(self.cells[self.cells.len() / 2 - 1].0.clone())
        } else {
            InsertResult::Ok
        }
    }

    pub fn split(&mut self, rchild: &mut BTreeNode, btree: &BTree) {
        // 鏍规嵁B*鏍戝畾涔夛紝鍙冲瓙鏍戝垎閰嶅埌children_pgno涓?澶氫簬涓€鍗?鐨勫€?
        let split = self.cells.len() / 2;

        // 灏哻ells[split]鍙婁互鍚庣殑鏁版嵁绉诲姩鍒皉child涓?姝ゆ椂rchild涓虹┖)
        let (l, r) = self.cells.split_at(split);
        let l = l.to_vec();
        let mut r = r.to_vec();
        rchild.cells.append(&mut r);
        self.cells = l;

        // 绉诲姩children_pgno
        if !self.children_pgno.is_empty() {
            let (l, r) = self.children_pgno.split_at(split);
            let l = l.to_vec();
            let mut r = r.to_vec();
            rchild.children_pgno.append(&mut r);
            self.children_pgno = l;
        }

        // 鏇存柊B*鏍戦摼琛ㄦ暟鎹?
        if self.right != 0 {
            let mut prev_right = btree.getnode(self.right);
            prev_right.left = rchild.pgno;
            rchild.right = self.right;
            prev_right.write_buf();
        }
        self.right = rchild.pgno;
        rchild.left = self.pgno;
        self.write_buf();
        rchild.write_buf();
    }

    fn remove_leaf(&mut self, key: &BTreeDataType, m: usize) -> RemoveResult {
        match self.binary_search_cell(key) {
            SearchResult::Equal(idx) => {
                self.cells.remove(idx);
                self.write_buf();
                if self.cells.len() < m {
                    RemoveResult::Needbalance
                } else {
                    RemoveResult::Ok
                }
            }
            SearchResult::Lessthan(_) => RemoveResult::Error,
            SearchResult::Right => RemoveResult::Error,
        }
    }

    fn find_leftsibling(&mut self, key: &BTreeDataType) -> Option<u32> {
        debug_assert!(!self.is_leaf);
        match self.binary_search_cell(key) {
            SearchResult::Equal(idx) => {
                if idx == 0 {
                    None
                } else {
                    Some(self.children_pgno[idx - 1])
                }
            }
            SearchResult::Lessthan(idx) => {
                if idx == 0 {
                    None
                } else {
                    Some(self.children_pgno[idx - 1])
                }
            }
            SearchResult::Right => Some(self.children_pgno[self.children_pgno.len() - 2]),
        }
    }

    fn find_rightsibling(&mut self, key: &BTreeDataType) -> Option<u32> {
        debug_assert!(!self.is_leaf);
        match self.binary_search_cell(key) {
            SearchResult::Equal(idx) => {
                if idx == self.children_pgno.len() - 1 {
                    None
                } else {
                    Some(self.children_pgno[idx + 1])
                }
            }
            SearchResult::Lessthan(idx) => {
                if idx == self.children_pgno.len() - 1 {
                    None
                } else {
                    Some(self.children_pgno[idx + 1])
                }
            }
            SearchResult::Right => None,
        }
    }

    fn need_balance(&self, m: usize) -> bool {
        self.cells.len() < m
    }

    fn try_borrow_left(
        &mut self,
        key: &BTreeDataType,
        node: &mut BTreeNode,
        btree: &BTree,
    ) -> bool {
        match self.find_leftsibling(&key) {
            Some(lsn) => {
                let mut ls = btree.getnode(lsn);
                if ls.cells.len() > btree.m {
                    let lastcell = ls.cells.pop().expect("noway");
                    node.cells.insert(0, lastcell);
                    if !node.is_leaf {
                        node.children_pgno
                            .insert(0, ls.children_pgno.pop().expect("noway"));
                    }
                    // 鏇存柊self鐨勭储寮曞€?
                    let midkey = ls.cells.last().expect("noway").0.clone();
                    match self.binary_search_cell(key) {
                        SearchResult::Equal(idx) => {
                            self.cells[idx - 1].0 = midkey.clone();
                        }
                        SearchResult::Lessthan(idx) => {
                            self.cells[idx - 1].0 = midkey.clone();
                        }
                        SearchResult::Right => {
                            self.cells.last_mut().expect("noway").0 = midkey.clone();
                        }
                    }
                    self.write_buf();
                    node.write_buf();
                    ls.write_buf();
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    fn try_borrow_right(
        &mut self,
        key: &BTreeDataType,
        node: &mut BTreeNode,
        btree: &BTree,
    ) -> bool {
        match self.find_rightsibling(key) {
            Some(rsn) => {
                let mut rs = btree.getnode(rsn);
                if rs.cells.len() > btree.m {
                    let firstcell = rs.cells[0].clone();
                    rs.cells.remove(0);
                    node.cells.push(firstcell);
                    if !node.is_leaf {
                        let pgno = rs.children_pgno[0];
                        rs.children_pgno.remove(0);
                        node.children_pgno.push(pgno);
                    }
                    // 鏇存柊self鐨勭储寮曞€?
                    let midkey = node.cells.last().expect("noway").0.clone();
                    match self.binary_search_cell(key) {
                        SearchResult::Equal(idx) => {
                            self.cells[idx].0 = midkey.clone();
                        }
                        SearchResult::Lessthan(idx) => {
                            self.cells[idx].0 = midkey.clone();
                        }
                        SearchResult::Right => panic!("noway"),
                    }
                    self.write_buf();
                    node.write_buf();
                    rs.write_buf();
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    fn clear(&mut self) {
        self.left = 0;
        self.right = 0;
        self.cells.clear();
        self.children_pgno.clear();
    }

    // node涓簂eaf鑺傜偣浣跨敤璇ュ嚱鏁?
    fn merge_leaf(
        &mut self,
        key: &BTreeDataType,
        node: &mut BTreeNode,
        btree: &BTree,
    ) -> Option<NodePtr> {
        if !node.is_leaf {
            return self.merge_internal(key, node, btree);
        }

        // 鏌ユ壘鍙敤鐨勫厔寮熻妭鐐?
        let lsibling = self.find_leftsibling(key);
        let rsibling = self.find_rightsibling(key);
        match (lsibling, rsibling) {
            (None, None) => panic!("Empty"),
            (Some(lsn), None) => {
                // 涓庡乏鍏勫紵鍚堝苟
                let mut ls = btree.getnode(lsn);
                let nodelast = node.cells.last().expect("noway").0.clone();
                ls.cells.append(&mut node.cells);
                // 鏇存柊self鐨刱ey
                match self.binary_search_cell(key) {
                    SearchResult::Equal(idx) => {
                        self.cells.remove(idx);
                        self.cells[idx - 1].0 = nodelast;
                        self.children_pgno.remove(idx);
                    }
                    SearchResult::Lessthan(idx) => {
                        self.cells.remove(idx);
                        self.cells[idx - 1].0 = nodelast;
                        self.children_pgno.remove(idx);
                    }
                    SearchResult::Right => {
                        self.cells.last_mut().expect("noway").0 = nodelast;
                        self.children_pgno.pop();
                    }
                }
                // 浣块〉闈㈢┖闂?
                ls.right = node.right;
                if ls.right != 0 {
                    let mut rnode = btree.getnode(ls.right);
                    rnode.left = ls.pgno;
                    rnode.write_buf();
                }
                node.clear();
                node.write_buf();
                btree.set_page_free(node.pgno);
                self.write_buf();
                ls.write_buf();
                Some(ls)
            }
            (None, Some(rsn)) | (Some(_), Some(rsn)) => {
                // 涓庡彸鍏勫紵鍚堝苟
                let mut rs = btree.getnode(rsn);
                let nodelast = rs.cells.last().expect("noway").0.clone();
                node.cells.append(&mut rs.cells);
                // 鏇存柊self鐨刱ey
                match self.binary_search_cell(key) {
                    SearchResult::Equal(idx) => {
                        self.cells.remove(idx);
                        if self.cells.is_empty() {
                            self.cells.insert(0, (nodelast, BTreeValueType::null()))
                        } else if self.cells.len() > idx {
                            self.cells[idx].0 = nodelast;
                        }
                        self.children_pgno.remove(idx + 1);
                    }
                    SearchResult::Lessthan(idx) => {
                        self.cells.remove(idx);
                        if self.cells.is_empty() {
                            self.cells.insert(0, (nodelast, BTreeValueType::null()))
                        } else if self.cells.len() > idx {
                            self.cells[idx].0 = nodelast;
                        }
                        self.children_pgno.remove(idx + 1);
                    }
                    SearchResult::Right => panic!("noway"),
                }
                // 浣块〉闈㈢┖闂?
                node.right = rs.right;
                if node.right != 0 {
                    let mut rnode = btree.getnode(node.right);
                    rnode.left = node.pgno;
                    rnode.write_buf();
                }
                rs.clear();
                rs.write_buf();
                btree.set_page_free(rs.pgno);
                self.write_buf();
                node.write_buf();
                None
            }
        }
    }

    // node涓篿nternal鑺傜偣浣跨敤璇ュ嚱鏁?
    fn merge_internal(
        &mut self,
        key: &BTreeDataType,
        node: &mut BTreeNode,
        btree: &BTree,
    ) -> Option<NodePtr> {
        // 鏌ユ壘鍙敤鐨勫厔寮熻妭鐐?
        let lsibling = self.find_leftsibling(key);
        let rsibling = self.find_rightsibling(key);
        match (lsibling, rsibling) {
            (None, None) => panic!("Empty"),
            (Some(lsn), None) => {
                // 涓庡乏鍏勫紵鍚堝苟
                let mut ls = btree.getnode(lsn);
                let nodelast = node.cells.last().expect("noway").0.clone();
                ls.cells.append(&mut node.cells);
                ls.children_pgno.append(&mut node.children_pgno);
                // 鏇存柊self鐨刱ey
                match self.binary_search_cell(key) {
                    SearchResult::Equal(idx) => {
                        self.cells.remove(idx);
                        self.cells[idx - 1].0 = nodelast;
                        self.children_pgno.remove(idx);
                    }
                    SearchResult::Lessthan(idx) => {
                        self.cells.remove(idx);
                        self.cells[idx - 1].0 = nodelast;
                        self.children_pgno.remove(idx);
                    }
                    SearchResult::Right => {
                        self.cells.last_mut().expect("noway").0 = nodelast;
                        self.children_pgno.pop();
                    }
                }
                // 浣块〉闈㈢┖闂?
                ls.right = node.right;
                if ls.right != 0 {
                    let mut rnode = btree.getnode(ls.right);
                    rnode.left = ls.pgno;
                    rnode.write_buf();
                }
                node.clear();
                node.write_buf();
                btree.set_page_free(node.pgno);
                self.write_buf();
                ls.write_buf();
                Some(ls)
            }
            (None, Some(rsn)) | (Some(_), Some(rsn)) => {
                // 涓庡彸鍏勫紵鍚堝苟
                let mut rs = btree.getnode(rsn);
                let nodelast = rs.cells.last().expect("noway").0.clone();
                node.cells.append(&mut rs.cells);
                node.children_pgno.append(&mut rs.children_pgno);
                // 鏇存柊self鐨刱ey
                match self.binary_search_cell(key) {
                    SearchResult::Equal(idx) => {
                        self.cells.remove(idx);
                        if self.cells.is_empty() {
                            self.cells.insert(0, (nodelast, BTreeValueType::null()))
                        } else if self.cells.len() > idx {
                            self.cells[idx].0 = nodelast;
                        }
                        self.children_pgno.remove(idx + 1);
                    }
                    SearchResult::Lessthan(idx) => {
                        self.cells.remove(idx);
                        if self.cells.is_empty() {
                            self.cells.insert(0, (nodelast, BTreeValueType::null()))
                        } else if self.cells.len() > idx {
                            self.cells[idx].0 = nodelast;
                        }
                        self.children_pgno.remove(idx + 1);
                    }
                    SearchResult::Right => panic!("noway"),
                }
                // 浣块〉闈㈢┖闂?
                node.right = rs.right;
                if node.right != 0 {
                    let mut rnode = btree.getnode(node.right);
                    rnode.left = node.pgno;
                    rnode.write_buf();
                }
                rs.clear();
                rs.write_buf();
                btree.set_page_free(rs.pgno);
                self.write_buf();
                node.write_buf();
                None
            }
        }
    }

    fn update_key_val(&mut self, key: &BTreeDataType, value: BTreeValueType) {
        match self.binary_search_cell(key) {
            SearchResult::Equal(idx) => self.cells[idx].1 = value,
            SearchResult::Lessthan(_) | SearchResult::Right => panic!("noway"),
        }
    }
}

enum InsertResult {
    Error,
    Needsplit(BTreeDataType),
    Ok,
}

enum RemoveResult {
    Error,
    Needbalance,
    Ok,
}

pub struct BTree {
    pager: Rc<Pager>,
    m: usize,
    is_write: Cell<bool>,
    new_free_page: RefCell<Vec<u32>>,
    freelist_first: Cell<u32>,
    freelist_len: Cell<u32>,
    total_pages: Cell<u32>,
}

pub const HEADER_FREELIST: usize = 0x20; // 4bytes
pub const HEADER_FREELIST_LEN: usize = 0x24; // 4bytes
pub const FREE_NEXTPTR: usize = 0x10; // 4bytes

impl BTree {
    pub fn get_freelist_len(&self) -> u32 {
        self.freelist_len.get()
    }

    pub fn get_freelist_first(&self) -> u32 {
        self.freelist_first.get()
    }

    pub fn new(pager: &Rc<Pager>, m: usize) -> Self {
        BTree {
            pager: pager.clone(),
            m,
            is_write: Cell::new(false),
            new_free_page: RefCell::new(Vec::new()),
            total_pages: Cell::new(pager.get_total_pages()),
            freelist_first: Cell::new(0),
            freelist_len: Cell::new(0),
        }
    }

    pub fn get_rootpage(&self) -> NodePtr {
        self.getnode(1)
        //self.newnode(true, true) // todo! 杩欒璇彞浠呯敤浜庤皟璇曪紝搴旇鐢?self.getnode(1) 鏉ヤ唬鏇?
    }

    pub fn set_page_free(&self, pgno: u32) {
        self.new_free_page.borrow_mut().push(pgno);
        self.new_free_page.borrow_mut().sort();
    }

    /// Recursively free every page in the subtree rooted at `root_pgno`.
    pub fn free_tree(&self, root_pgno: u32) {
        let node = self.getnode(root_pgno);
        if !node.is_leaf_node() {
            for &child_pgno in node.children_pgno() {
                if child_pgno != 0 {
                    self.free_tree(child_pgno);
                }
            }
        }
        self.set_page_free(root_pgno);
    }

    pub fn get_newpage(&self) -> PagePtr {
        match self.acquire_free_pgno() {
            Some(pgno) => {
                // 瀛樺湪绌洪棽椤甸潰锛屼娇鐢ㄥ凡瀛樺湪鐨勭┖闂查〉闈?
                let page = self.pager.fetch(pgno).expect("Memory stress");
                page
            }
            None => {
                // 涓嶅瓨鍦ㄧ┖闂查〉闈紝鍒嗛厤涓€涓柊椤甸潰
                let required_pgno = self.total_pages.get();
                self.total_pages.set(self.total_pages.get() + 1);
                let page = self.pager.fetch(required_pgno).expect("Memory stress");
                self.pager.extend_page_count(required_pgno + 1);
                page
            }
        }
    }

    pub fn read_freepage_metadata(&self, pgno: u32) -> u32 {
        let pageptr = self.pager.fetch(pgno).expect("noway");
        let mut page = pageptr.borrow_mut();
        let pdata = page.data_read();
        let mut next_bytes = [0u8; 4];
        next_bytes.copy_from_slice(&pdata[FREE_NEXTPTR..FREE_NEXTPTR + 4]);
        u32::from_be_bytes(next_bytes)
    }

    pub fn acquire_free_pgno(&self) -> Option<u32> {
        let is_empty = self.new_free_page.borrow_mut().is_empty();
        if is_empty {
            // 浜嬪姟娌℃湁浜х敓绌洪棽椤甸潰锛屽皾璇曟煡鎵炬暟鎹簱鏂囦欢涓師鏈夌殑绌洪棽椤甸潰
            if self.freelist_len.get() == 0 {
                None
            } else {
                // 鑾峰彇鏁版嵁搴撴枃浠朵腑鍘熸湁鐨勭┖闂查〉闈?
                let pgno = self.freelist_first.get();
                let next_page = self.read_freepage_metadata(pgno);
                self.freelist_first.set(next_page);
                self.freelist_len.set(self.freelist_len.get() - 1);
                Some(pgno)
            }
        } else {
            Some(self.new_free_page.borrow_mut().pop().expect("noway"))
        }
    }

    pub fn getnode(&self, pgno: u32) -> NodePtr {
        match self.pager.fetch(pgno) {
            Some(p) => {
                let flags = p.borrow_mut().flags();
                if (flags & PAGE_EMPTY) != 0 {
                    panic!("Unexpected page {pgno} with flags {flags}");
                }
                let node = NodePtr::new(BTreeNode::load_page(&self.pager, &p));
                node
            }
            None => panic!("Memory stress"),
        }
    }

    pub fn newnode(&self, is_leaf: bool, is_data_table: bool) -> NodePtr {
        let page = self.get_newpage();
        let node: NodePtr = NodePtr::new(BTreeNode::load_empty_page(
            &self.pager,
            &page,
            is_leaf,
            is_data_table,
        ));
        node
    }

    pub fn write_freepage(&self) {
        // 灏唂reepage缁勭粐杩涙暟鎹簱鏂囦欢
        let header_page = self.pager.fetch(0).expect("noway");
        self.pager.will_modify(&header_page);
        let mut page = header_page.borrow_mut();
        let pdata = page.data_write();

        // 鍐檔ew_free_page闃熷垪涓殑椤甸潰
        let isempty = self.new_free_page.borrow_mut().is_empty();
        if !isempty {
            let new_free_page = self.new_free_page.borrow_mut();
            let len = new_free_page.len();
            for i in 0..len {
                let pageref = self.pager.fetch(new_free_page[i]).expect("noway");
                self.pager.will_modify(&pageref);
                let mut page = pageref.borrow_mut();
                let ppdata = page.data_write();
                ppdata.copy_from_slice(&[0u8; 4096]);

                ppdata[0] = 2;
                if i < len - 1 {
                    ppdata[FREE_NEXTPTR..FREE_NEXTPTR + 4]
                        .copy_from_slice(&u32::to_be_bytes(new_free_page[i + 1]));
                } else {
                    if self.freelist_len.get() != 0 {
                        ppdata[FREE_NEXTPTR..FREE_NEXTPTR + 4]
                            .copy_from_slice(&u32::to_be_bytes(self.freelist_first.get()));
                    } else {
                        ppdata[FREE_NEXTPTR..FREE_NEXTPTR + 4]
                            .copy_from_slice(&u32::to_be_bytes(0));
                    }
                }
            }

            self.freelist_len.set(self.freelist_len.get() + len as u32);
            self.freelist_first.set(new_free_page[0]);
        }
        pdata[HEADER_FREELIST..HEADER_FREELIST + 4]
            .copy_from_slice(&self.freelist_first.get().to_be_bytes());
        pdata[HEADER_FREELIST_LEN..HEADER_FREELIST_LEN + 4]
            .copy_from_slice(&self.freelist_len.get().to_be_bytes());
    }

    pub fn begin_transaction_read(&self) {
        self.pager.begin_transaction_read();
        self.total_pages.set(self.pager.get_total_pages());
        let header_page = self.pager.fetch(0).expect("noway");
        let mut page = header_page.borrow_mut();
        let pdata = page.data_read();
        let mut freelist_bytes = [0u8; 4];
        freelist_bytes.copy_from_slice(&pdata[HEADER_FREELIST..HEADER_FREELIST + 4]);
        self.freelist_first.set(u32::from_be_bytes(freelist_bytes));
        let mut freelist_len_bytes = [0u8; 4];
        freelist_len_bytes.copy_from_slice(&pdata[HEADER_FREELIST_LEN..HEADER_FREELIST_LEN + 4]);
        self.freelist_len
            .set(u32::from_be_bytes(freelist_len_bytes));
    }

    pub fn begin_transaction_write(&self) {
        self.pager.begin_transaction_write();
        self.new_free_page.borrow_mut().clear();
        self.is_write.set(true);
    }

    pub fn end_transaction_write(&self) {
        self.write_freepage();
        self.pager.end_transaction_write();
        self.is_write.set(false);
    }

    pub fn end_transaction_read(&self) {
        self.pager.end_transaction_read();
    }

    pub fn rollback(&self) {
        self.new_free_page.borrow_mut().clear();
        self.pager.check_and_tryrollback();
    }

    fn refresh_separators(&self) {
        let _ = self.refresh_separators_inner(1);
    }

    fn refresh_separators_inner(&self, pgno: u32) -> Option<BTreeDataType> {
        let mut node = self.getnode(pgno);

        if node.is_leaf {
            return node.cells.last().map(|(key, _)| key.clone());
        }

        let children = node.children_pgno.clone();
        let mut child_maxes = Vec::new();
        for child_pgno in children {
            if let Some(max_key) = self.refresh_separators_inner(child_pgno) {
                child_maxes.push(max_key);
            }
        }

        let mut changed = false;
        for idx in 0..node.cells.len() {
            if let Some(max_key) = child_maxes.get(idx) {
                if node.cells[idx].0 != *max_key {
                    node.cells[idx].0 = max_key.clone();
                    changed = true;
                }
            }
        }

        if changed {
            node.write_buf();
        }

        child_maxes.last().cloned()
    }

    pub fn create_db(&self) {
        self.pager.begin_transaction_read();
        self.pager.begin_transaction_write();
        self.is_write.set(true);
        self.total_pages.set(2); // 澶碢age+root tree鏍筽age
        let root_page = self.pager.fetch(1).expect("Memory Stress");
        self.pager.extend_page_count(2);
        let mut node = NodePtr::new(BTreeNode::load_empty_page(
            &self.pager,
            &root_page,
            true,
            true,
        ));
        node.write_buf();
    }
}

pub const TREE_MAX_DEPTH: usize = 20;

/// B鏍戞父鏍?
pub struct BTreeCursor {
    pager: Rc<Pager>,
    is_data: bool,
    is_write: bool,
    btree: Rc<BTree>,
    treeid: u32,
    node: NodePtr,
    path: VecDeque<NodePtr>,
    cell_idx: usize,
}

enum SearchResult {
    Equal(usize),    // Equal(index of cellref array)
    Lessthan(usize), // Lessthan(index of cellref array)
    Right,           // 鍦ㄨ妭鐐圭殑鏈€鍙冲瓙鏍戜笂
}

impl BTreeCursor {
    pub fn new(btree: &Rc<BTree>, treeid: u32, rootpgno: u32) -> Self {
        BTreeCursor {
            pager: btree.pager.clone(),
            is_data: true,
            is_write: true,
            btree: btree.clone(),
            treeid,
            node: btree.getnode(rootpgno),
            path: VecDeque::new(),
            cell_idx: 0,
        }
    }

    pub fn new_root(btree: &Rc<BTree>) -> Self {
        BTreeCursor {
            pager: btree.pager.clone(),
            is_data: true,
            is_write: true,
            btree: btree.clone(),
            treeid: 1,
            node: btree.get_rootpage().clone(),
            path: VecDeque::new(),
            cell_idx: 0,
        }
    }

    pub fn moveto_root(&mut self) {
        while let Some(p) = self.path.pop_back() {
            self.node = p.clone();
        }
    }

    pub fn moveto_leftmost(&mut self) {
        while !self.node.is_leaf {
            let node = self.btree.getnode(self.node.get_leftmost_child());
            self.node = node;
        }
        self.cell_idx = 0;
    }

    pub fn moveto_rightmost(&mut self) {
        while !self.node.is_leaf {
            let node = self.btree.getnode(self.node.get_rightmost_child());
            self.node = node;
        }
        self.cell_idx = self.node.cells.len();
    }

    pub fn moveto_first_entry(&mut self) -> bool {
        self.moveto_leftmost();
        !self.node.cells.is_empty()
    }

    pub fn moveto_next_entry(&mut self) -> bool {
        if self.node.cells.is_empty() {
            return false;
        }

        if self.cell_idx + 1 < self.node.cells.len() {
            self.cell_idx += 1;
            return true;
        }

        if self.node.right != 0 {
            self.node = self.btree.getnode(self.node.right);
            self.cell_idx = 0;
            return !self.node.cells.is_empty();
        }

        self.cell_idx = self.node.cells.len();
        false
    }

    pub fn moveto_key_entry(&mut self, key: &BTreeDataType) -> bool {
        if !self.moveto_target(key) {
            return false;
        }

        if let Some(idx) = self
            .node
            .cells
            .iter()
            .position(|(cell_key, _)| cell_key == key)
        {
            self.cell_idx = idx;
            true
        } else {
            false
        }
    }

    pub fn current_entry(&self) -> Option<(BTreeDataType, BTreeValueType)> {
        self.node
            .cells
            .get(self.cell_idx)
            .map(|(key, value)| (key.clone(), value.clone()))
    }

    pub fn next_integer_key(&mut self) -> Option<u64> {
        self.moveto_rightmost();
        match self.prev() {
            Some((BTreeDataType::Integer(key), _)) => key.checked_add(1),
            Some(_) => None,
            None => Some(1),
        }
    }

    pub fn moveto_child(&mut self, pgno: u32) {
        self.path.push_back(self.node.clone());
        self.node = self.btree.getnode(pgno);
    }

    pub fn moveto_parent(&mut self) {
        if let Some(p) = self.path.pop_back() {
            self.node = p.clone();
        }
    }

    pub fn print_tree(&mut self) {
        self.moveto_root();
        self.print_tree_inner(0);
    }

    pub fn print_tree_inner(&mut self, depth: u32) {
        for _ in 0..depth {
            print!(" ");
        }
        print!("{}:Page {} ", depth, self.node.pgno);
        if self.node.is_leaf {
            print!("leaf left={} right={} ", self.node.left, self.node.right);
            print!("kv:{{");
            for (k, v) in &self.node.cells {
                print!("[{},{}],", k, v);
            }
            print!("}}\n");
        } else {
            print!(
                "intr left={} right={} keys:{{",
                self.node.left, self.node.right
            );
            for (k, _) in &self.node.cells {
                print!("[{}],", k);
            }
            print!("}} ");

            let children = self.node.children_pgno.clone();
            print!("kids:{{");
            for i in &children {
                print!("{},", *i);
            }
            print!("}}\n");
            for i in children {
                self.moveto_child(i);
                self.print_tree_inner(depth + 1);
            }
        }
        self.moveto_parent();
    }

    pub fn moveto_target(&mut self, key: &BTreeDataType) -> bool {
        // 褰撳墠cell鍚湁闇€瑕佺殑key锛屼笖涓哄彾瀛愰〉闈?
        if self.node.contains_key(key) && self.node.is_leaf {
            return true;
        }

        // 褰撳墠cell涓嶅惈鏈夋墍闇€瑕佺殑key锛岀Щ鍔ㄥ埌鏍硅妭鐐硅繘琛屾煡鎵?
        self.moveto_root();
        loop {
            match self.node.get_key_searchresult(key) {
                SearchResult::Equal(idx) => {
                    if self.node.is_leaf {
                        // 绉诲姩鍒板彾瀛愯妭鐐癸紝鍙互杩斿洖
                        break true;
                    } else {
                        // 闈炲彾瀛愯妭鐐癸紝浣嗘槸key鍦ㄨ椤甸潰澶勶紝鏍规嵁B*鏍戝畾涔夛紝杩唬鏌ユ壘宸﹀瓙鏍?
                        let rchild = self.node.get_leftchild(idx);
                        self.moveto_child(rchild);
                    }
                }
                SearchResult::Lessthan(idx) => {
                    if self.node.is_leaf {
                        // 鍙跺瓙鑺傜偣锛屼絾鏌ユ壘涓嶅埌key锛屾煡鎵惧け璐ワ紝key涓嶅瓨鍦?
                        break false;
                    }
                    // 闈炲彾瀛愯妭鐐癸紝鏍规嵁B*鏍戝畾涔夛紝杩唬鏌ユ壘宸﹀瓙鏍?
                    let lchild = self.node.get_leftchild(idx);
                    self.moveto_child(lchild);
                }
                SearchResult::Right => {
                    if self.node.is_leaf {
                        // 鍙跺瓙鑺傜偣锛屼絾鏌ユ壘涓嶅埌key锛屾煡鎵惧け璐ワ紝key涓嶅瓨鍦?
                        break false;
                    }
                    // 闈炲彾瀛愯妭鐐癸紝鍦ㄦ墍鏈塩ell鐨勫彸杈癸紝杩唬鏌ユ壘鏈€鍙冲瓙鏍?
                    let rchild = self.node.get_rightmost_child();
                    self.moveto_child(rchild);
                }
            };
        }
    }

    pub fn insert(&mut self, key: &BTreeDataType, value: &BTreeValueType) -> bool {
        if self.moveto_target(key) {
            // insert鎿嶄綔閬囧埌浜嗙浉鍚岀殑key
            false
        } else {
            match self.node.insert_leaf(key, value, self.btree.m) {
                InsertResult::Error => {
                    return false;
                }
                InsertResult::Needsplit(upkey) => {
                    let mut newpage = self.btree.newnode(true, self.is_data);
                    self.node.split(&mut newpage, &self.btree);
                    let lchild = self.node.clone();
                    self.split_inner(&lchild, &mut newpage, &upkey);
                    // 閲嶆柊鐢熸垚path
                    self.moveto_target(key);
                }
                InsertResult::Ok => {}
            }
            true
        }
    }

    fn keep_root(&mut self, newnode: &mut NodePtr, rchild: &mut NodePtr, upkey: &BTreeDataType) {
        newnode.left = self.node.left;
        newnode.right = self.node.right;
        newnode.cells = self.node.cells.clone();
        newnode.children_pgno = self.node.children_pgno.clone();
        self.node.left = 0;
        self.node.right = 0;
        self.node.cells.clear();
        self.node.is_leaf = false;
        self.node.children_pgno.clear();
        self.node
            .insert_internal(upkey, newnode.pgno, rchild.pgno, self.btree.m);
        rchild.left = newnode.pgno;
        self.node.write_buf();
        newnode.write_buf();
        rchild.write_buf();
    }

    fn split_inner(&mut self, lchild: &NodePtr, rchild: &mut NodePtr, upkey: &BTreeDataType) {
        if self.path.is_empty() {
            // 璇ヨ妭鐐规槸鏍硅妭鐐?
            let mut newroot = self.btree.newnode(self.node.is_leaf, self.is_data);
            self.keep_root(&mut newroot, rchild, upkey);
        } else {
            // 璇ヨ妭鐐逛笉鏄牴鑺傜偣
            self.moveto_parent();
            match self
                .node
                .insert_internal(upkey, lchild.pgno, rchild.pgno, self.btree.m)
            {
                InsertResult::Error => panic!("Err insert"),
                InsertResult::Needsplit(upkey) => {
                    let mut newpage = self.btree.newnode(false, self.is_data);
                    self.node.split(&mut newpage, &self.btree);
                    let lchild = self.node.clone();
                    self.split_inner(&lchild, &mut newpage, &upkey);
                }
                InsertResult::Ok => {
                    // 鍒嗚瀹屾垚锛屼笉闇€瑕佸共鍒殑浜嗭紝璧板洖root
                    self.moveto_root();
                }
            }
        }
    }

    pub fn remove(&mut self, key: &BTreeDataType) -> bool {
        if !self.moveto_target(key) {
            false
        } else {
            let removed_key_was_leaf_max = self
                .node
                .cells
                .last()
                .map(|(last_key, _)| last_key == key)
                .unwrap_or(false);

            match self.node.remove_leaf(key, self.btree.m) {
                RemoveResult::Error => {
                    return false;
                }
                RemoveResult::Needbalance => {
                    self.remove_balance_clean(key);
                }
                RemoveResult::Ok => {
                    if removed_key_was_leaf_max {
                        if let Some((new_max, _)) = self.node.cells.last() {
                            self.update_parent_separator(key, &new_max.clone());
                        }
                    }
                    // 鍒犻櫎瀹屾垚锛屼笉闇€瑕佸共鍒殑浜?
                }
            }
            self.btree.refresh_separators();
            true
        }
    }

    fn update_parent_separator(&mut self, old_key: &BTreeDataType, new_key: &BTreeDataType) {
        for parent in self.path.iter_mut().rev() {
            if let SearchResult::Equal(idx) = parent.binary_search_cell(old_key) {
                parent.cells[idx].0 = new_key.clone();
                parent.write_buf();
            }
        }
    }

    fn remove_balance_clean(&mut self, key: &BTreeDataType) {
        if self.path.is_empty() {
            return;
        }

        let mut node = self.node.clone();
        self.moveto_parent();

        if self.node.try_borrow_left(key, &mut node, &self.btree) {
            return;
        }

        if self.node.try_borrow_right(key, &mut node, &self.btree) {
            return;
        }

        let _ = self.node.merge_leaf(key, &mut node, &self.btree);

        if self.node.need_balance(self.btree.m) {
            self.balance_internal_clean(key);
        }
    }

    fn remove_balance(&mut self, key: &BTreeDataType) {
        if !self.path.is_empty() {
            // 璇ラ〉闈笉涓烘牴椤甸潰锛屽皾璇曞钩琛?
            let mut node = self.node.clone();
            self.moveto_parent();
            if !self.node.try_borrow_left(key, &mut node, &self.btree) {
                // 鍊熺敤宸﹀厔寮熷け璐ワ紝灏濊瘯鍊熺敤鍙冲厔寮?
                if !self.node.try_borrow_right(key, &mut node, &self.btree) {
                    // 鍊熺敤鍙冲厔寮熷け璐ワ紝鍚堝苟鑺傜偣锛屽悓鏃秔arent鑺傜偣闇€瑕佽繘琛屽钩琛?
                    match self.node.merge_leaf(key, &mut node, &self.btree) {
                        Some(_) => {}
                        None => {}
                    }
                    if self.node.need_balance(self.btree.m) {
                        self.balance_internal_clean(key);
                    }
                }
            }
        }
    }

    fn decr_root(&mut self, child_pgno: u32) {
        let mut cnode = self.btree.getnode(child_pgno);
        self.node.is_leaf = cnode.is_leaf;
        self.node.left = 0;
        self.node.right = 0;
        self.node.cells = cnode.cells.clone();
        self.node.children_pgno = cnode.children_pgno.clone();
        cnode.cells.clear();
        cnode.children_pgno.clear();
        self.node.write_buf();
        cnode.write_buf();
    }

    fn balance_internal_clean(&mut self, key: &BTreeDataType) {
        if self.path.is_empty() {
            if self.node.children_pgno.len() == 1 {
                let child = self.node.children_pgno[0];
                self.decr_root(child);
                self.btree.set_page_free(child);
            }
            return;
        }

        let mut node = self.node.clone();
        self.moveto_parent();

        if self.node.try_borrow_left(key, &mut node, &self.btree) {
            return;
        }

        if self.node.try_borrow_right(key, &mut node, &self.btree) {
            return;
        }

        let _ = self.node.merge_internal(key, &mut node, &self.btree);

        if self.node.need_balance(self.btree.m) {
            self.balance_internal_clean(key);
        }
    }

    fn balance_internal(&mut self, key: &BTreeDataType) {
        if !self.path.is_empty() {
            // 璇ラ〉闈笉涓烘牴椤甸潰锛屽皾璇曞钩琛?
            let mut node = self.node.clone();
            self.moveto_parent();
            if !self.node.try_borrow_left(key, &mut node, &self.btree) {
                // 鍊熺敤宸﹀厔寮熷け璐ワ紝灏濊瘯鍊熺敤鍙冲厔寮?
                if !self.node.try_borrow_right(key, &mut node, &self.btree) {
                    // 鍊熺敤鍙冲厔寮熷け璐ワ紝鍚堝苟鑺傜偣锛屽悓鏃秔arent鑺傜偣闇€瑕佽繘琛屽钩琛?
                    match self.node.merge_internal(key, &mut node, &self.btree) {
                        Some(_) => {}
                        None => {}
                    }
                    if self.node.need_balance(self.btree.m) {
                        self.balance_internal(key);
                    }
                }
            }
        } else {
            // 璇ヨ妭鐐逛负鏍硅妭鐐癸紝闇€瑕侀檷浣庢爲鐨勯珮搴?
            if self.node.children_pgno.len() == 1 {
                let child = self.node.children_pgno[0];
                self.decr_root(child);
                self.btree.set_page_free(child);
            }
        }
    }

    pub fn get_val(&mut self, key: &BTreeDataType) -> Option<BTreeValueType> {
        match self.moveto_target(key) {
            true => self.node.get_key(key),
            false => None,
        }
    }
    pub fn prev(&mut self) -> Option<(BTreeDataType, BTreeValueType)> {
        if self.node.cells.is_empty() {
            return None;
        }
        if self.cell_idx == 0 {
            if self.node.left != 0 {
                self.node = self.btree.getnode(self.node.left);
                self.cell_idx = self.node.cells.len();
            } else {
                return None;
            }
        }
        self.cell_idx -= 1;
        let (k, v) = self.node.cells[self.cell_idx].clone();
        Some((k, v))
    }

    pub fn next(&mut self) -> Option<(BTreeDataType, BTreeValueType)> {
        if self.cell_idx < self.node.cells.len() {
            let (k, v) = self.node.cells[self.cell_idx].clone();
            self.cell_idx += 1;
            return Some((k, v));
        }
        if self.node.right != 0 {
            self.node = self.btree.getnode(self.node.right);
            self.cell_idx = 0;
            if !self.node.cells.is_empty() {
                let (k, v) = self.node.cells[0].clone();
                self.cell_idx = 1;
                return Some((k, v));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, rc::Rc, time::SystemTime};

    fn temp_db_path(test_name: &str) -> String {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("minisqlr_{test_name}_{nonce}.db"))
            .to_string_lossy()
            .into_owned()
    }

    fn cleanup_db(path: &str) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(format!("{path}-journal"));
    }

    fn create_btree(path: &str, m: usize) -> Rc<BTree> {
        let db = Pager::create_or_opendb(path.to_owned());
        let pager = Rc::new(Pager::new(db, path.to_owned()));
        let btree = Rc::new(BTree::new(&pager, m));
        btree.create_db();
        btree
    }

    fn open_btree(path: &str, m: usize) -> Rc<BTree> {
        let db = Pager::create_or_opendb(path.to_owned());
        let pager = Rc::new(Pager::new(db, path.to_owned()));
        let btree = Rc::new(BTree::new(&pager, m));
        btree.begin_transaction_read();
        btree
    }

    #[test]
    fn null_values_round_trip() {
        let value = BTreeValueType {
            v: vec![BTreeDataType::Null, BTreeDataType::Integer(7)],
        };

        let decoded = BTreeValueType::new(&value.get_bytes());

        assert!(matches!(decoded.v[0], BTreeDataType::Null));
        assert!(matches!(decoded.v[1], BTreeDataType::Integer(7)));
    }

    #[test]
    fn first_leaf_insert_is_persisted() {
        let path = temp_db_path("first_insert");
        cleanup_db(&path);

        let btree = create_btree(&path, 2);
        {
            {
                let mut cursor = BTreeCursor::new_root(&btree);
                assert!(cursor.insert(
                    &BTreeDataType::Integer(42),
                    &BTreeValueType::new_single(BTreeDataType::Integer(9)),
                ));
            }
            btree.end_transaction_write();
        }

        {
            let btree = open_btree(&path, 2);
            let mut cursor = BTreeCursor::new_root(&btree);
            assert!(cursor.get_val(&BTreeDataType::Integer(42)).is_some());
            btree.end_transaction_read();
        }

        cleanup_db(&path);
    }

    #[test]
    fn deleting_leaf_max_updates_parent_separator() {
        let path = temp_db_path("delete_parent_separator");
        cleanup_db(&path);

        {
            let btree = create_btree(&path, 2);
            {
                let mut cursor = BTreeCursor::new_root(&btree);
                for key in [1, 2, 3, 4, 0] {
                    assert!(cursor.insert(
                        &BTreeDataType::Integer(key),
                        &BTreeValueType::new_single(BTreeDataType::Integer(key + 100)),
                    ));
                }

                assert!(cursor.remove(&BTreeDataType::Integer(2)));
            }

            let root = btree.get_rootpage();
            assert!(matches!(root.cells[0].0, BTreeDataType::Integer(1)));
            btree.end_transaction_write();
        }

        cleanup_db(&path);
    }

    #[test]
    fn deleting_underfull_leaf_keeps_remaining_keys_searchable() {
        let path = temp_db_path("delete_rebalance");
        cleanup_db(&path);

        {
            let btree = create_btree(&path, 2);
            {
                let mut cursor = BTreeCursor::new_root(&btree);
                for key in [1, 2, 3, 4] {
                    assert!(cursor.insert(
                        &BTreeDataType::Integer(key),
                        &BTreeValueType::new_single(BTreeDataType::Integer(key + 100)),
                    ));
                }

                assert!(cursor.remove(&BTreeDataType::Integer(2)));
                assert!(cursor.get_val(&BTreeDataType::Integer(1)).is_some());
                assert!(cursor.get_val(&BTreeDataType::Integer(2)).is_none());
                assert!(cursor.get_val(&BTreeDataType::Integer(3)).is_some());
                assert!(cursor.get_val(&BTreeDataType::Integer(4)).is_some());
            }
            btree.end_transaction_write();
        }

        cleanup_db(&path);
    }

    #[test]
    fn mixed_insert_remove_orders_round_trip() {
        for round in 0..20 {
            let path = temp_db_path(&format!("mixed_orders_{round}"));
            cleanup_db(&path);

            let mut expected = [false; 40];
            let insert_order = shuffled_keys(40, round as u64 * 17 + 5);
            let remove_order = shuffled_keys(40, round as u64 * 31 + 11);

            {
                let btree = create_btree(&path, 2);
                {
                    let mut cursor = BTreeCursor::new_root(&btree);

                    for key in insert_order {
                        assert!(cursor.insert(
                            &BTreeDataType::Integer(key),
                            &BTreeValueType::new_single(BTreeDataType::Integer(key + 1000)),
                        ));
                        expected[key as usize] = true;
                    }

                    for key in 0..40 {
                        assert!(!cursor.insert(
                            &BTreeDataType::Integer(key),
                            &BTreeValueType::new_single(BTreeDataType::Integer(key + 2000)),
                        ));
                    }

                    for key in remove_order.iter().take(23) {
                        assert!(cursor.remove(&BTreeDataType::Integer(*key)));
                        expected[*key as usize] = false;
                    }

                    for key in 0..40 {
                        assert_eq!(
                            cursor.get_val(&BTreeDataType::Integer(key)).is_some(),
                            expected[key as usize],
                            "round {round}, key {key}"
                        );
                    }
                }
                btree.end_transaction_write();
            }

            {
                let btree = open_btree(&path, 2);
                let mut cursor = BTreeCursor::new_root(&btree);
                for key in 0..40 {
                    assert_eq!(
                        cursor.get_val(&BTreeDataType::Integer(key)).is_some(),
                        expected[key as usize],
                        "persisted round {round}, key {key}"
                    );
                }
                btree.end_transaction_read();
            }

            cleanup_db(&path);
        }
    }

    #[test]
    fn structural_invariants_survive_larger_random_workload() {
        for round in 0..12 {
            let path = temp_db_path(&format!("structural_{round}"));
            cleanup_db(&path);

            let mut expected = vec![false; 96];
            let mut ops = shuffled_keys(240, round as u64 * 97 + 23);

            {
                let btree = create_btree(&path, 2);
                {
                    let mut cursor = BTreeCursor::new_root(&btree);

                    for key in shuffled_keys(96, round as u64 * 41 + 7) {
                        assert!(cursor.insert(
                            &BTreeDataType::Integer(key),
                            &BTreeValueType::new_single(BTreeDataType::Integer(key + 3000)),
                        ));
                        expected[key as usize] = true;
                        assert_tree_matches(
                            &btree,
                            &expected,
                            &format!("round {round} insert {key}"),
                        );
                    }

                    for step in 0..ops.len() {
                        let key = ops[step] % 96;
                        if (ops[step].wrapping_add(round as u64) % 3) == 0 {
                            assert_eq!(
                                cursor.remove(&BTreeDataType::Integer(key)),
                                expected[key as usize],
                                "round {round}, step {step}, remove {key}"
                            );
                            expected[key as usize] = false;
                        } else {
                            assert_eq!(
                                cursor.insert(
                                    &BTreeDataType::Integer(key),
                                    &BTreeValueType::new_single(BTreeDataType::Integer(key + 4000)),
                                ),
                                !expected[key as usize],
                                "round {round}, step {step}, insert {key}"
                            );
                            expected[key as usize] = true;
                        }

                        assert_tree_matches(
                            &btree,
                            &expected,
                            &format!("round {round} step {step}"),
                        );
                    }
                }
                btree.end_transaction_write();
            }

            {
                let btree = open_btree(&path, 2);
                assert_tree_matches(&btree, &expected, &format!("round {round} persisted"));
                btree.end_transaction_read();
            }

            cleanup_db(&path);
            ops.clear();
        }
    }

    #[test]
    fn removing_all_keys_leaves_tree_reusable() {
        let path = temp_db_path("remove_all_reuse");
        cleanup_db(&path);

        {
            let btree = create_btree(&path, 2);
            let mut expected = vec![false; 64];
            {
                let mut cursor = BTreeCursor::new_root(&btree);
                for key in shuffled_keys(64, 71) {
                    assert!(cursor.insert(
                        &BTreeDataType::Integer(key),
                        &BTreeValueType::new_single(BTreeDataType::Integer(key + 5000)),
                    ));
                    expected[key as usize] = true;
                }
                assert_tree_matches(&btree, &expected, "remove_all inserted");

                for key in shuffled_keys(64, 131) {
                    assert!(cursor.remove(&BTreeDataType::Integer(key)));
                    expected[key as usize] = false;
                    assert_tree_matches(&btree, &expected, &format!("remove_all remove {key}"));
                }

                for key in shuffled_keys(64, 197) {
                    assert!(cursor.insert(
                        &BTreeDataType::Integer(key),
                        &BTreeValueType::new_single(BTreeDataType::Integer(key + 6000)),
                    ));
                    expected[key as usize] = true;
                    assert_tree_matches(&btree, &expected, &format!("remove_all reinsert {key}"));
                }
            }
            btree.end_transaction_write();
        }

        {
            let btree = open_btree(&path, 2);
            assert_tree_matches(&btree, &vec![true; 64], "remove_all persisted");
            btree.end_transaction_read();
        }

        cleanup_db(&path);
    }

    #[test]
    fn rollback_uses_current_transaction_backups() {
        let path = temp_db_path("rollback_current_transaction");
        cleanup_db(&path);

        let btree = create_btree(&path, 2);
        {
            {
                let mut cursor = BTreeCursor::new_root(&btree);
                assert!(cursor.insert(
                    &BTreeDataType::Integer(1),
                    &BTreeValueType::new_single(BTreeDataType::Integer(10)),
                ));
            }
            btree.end_transaction_write();
        }

        {
            btree.begin_transaction_write();
            {
                let mut cursor = BTreeCursor::new_root(&btree);
                assert!(cursor.insert(
                    &BTreeDataType::Integer(2),
                    &BTreeValueType::new_single(BTreeDataType::Integer(20)),
                ));
            }
            for idx in 0..32 {
                let mut node = btree.newnode(true, true);
                node.cells.push((
                    BTreeDataType::Integer(10_000 + idx),
                    BTreeValueType::new_single(BTreeDataType::Integer(idx)),
                ));
                node.write_buf();
            }
        }
        drop(btree);

        {
            let btree = open_btree(&path, 2);
            let mut cursor = BTreeCursor::new_root(&btree);
            assert!(
                cursor.get_val(&BTreeDataType::Integer(1)).is_some(),
                "committed key should survive rollback"
            );
            assert!(
                cursor.get_val(&BTreeDataType::Integer(2)).is_none(),
                "uncommitted key should be rolled back"
            );
            btree.end_transaction_read();
        }
        assert_eq!(
            fs::metadata(&path).expect("db metadata").len(),
            (2 * PAGE_SIZE) as u64,
            "rollback should remove pages allocated by the aborted transaction"
        );

        cleanup_db(&path);
    }

    #[test]
    fn rollback_discards_dirty_cached_pages() {
        let path = temp_db_path("rollback_cached_pages");
        cleanup_db(&path);

        let btree = create_btree(&path, 2);
        {
            let mut cursor = BTreeCursor::new_root(&btree);
            assert!(cursor.insert(
                &BTreeDataType::Integer(1),
                &BTreeValueType::new_single(BTreeDataType::Integer(10)),
            ));
        }
        btree.end_transaction_write();

        btree.begin_transaction_write();
        {
            let mut cursor = BTreeCursor::new_root(&btree);
            assert!(cursor.insert(
                &BTreeDataType::Integer(2),
                &BTreeValueType::new_single(BTreeDataType::Integer(20)),
            ));
        }

        btree.rollback();

        {
            let mut cursor = BTreeCursor::new_root(&btree);
            assert!(cursor.get_val(&BTreeDataType::Integer(1)).is_some());
            assert!(
                cursor.get_val(&BTreeDataType::Integer(2)).is_none(),
                "rollback should discard uncommitted cached pages"
            );
        }

        cleanup_db(&path);
    }

    struct TreeScan {
        keys: Vec<u64>,
        leaves: Vec<u32>,
    }

    fn assert_tree_matches(btree: &Rc<BTree>, expected: &[bool], context: &str) {
        let scan = scan_node(btree, 1, true, context);
        let expected_keys: Vec<u64> = expected
            .iter()
            .enumerate()
            .filter_map(|(key, present)| present.then_some(key as u64))
            .collect();

        assert_eq!(scan.keys, expected_keys, "{context}");

        if scan.leaves.len() > 1 {
            for pair in scan.leaves.windows(2) {
                let left = btree.getnode(pair[0]);
                let right = btree.getnode(pair[1]);
                assert_eq!(left.right, pair[1], "{context}: leaf right link");
                assert_eq!(right.left, pair[0], "{context}: leaf left link");
            }
        }
    }

    fn scan_node(btree: &Rc<BTree>, pgno: u32, is_root: bool, context: &str) -> TreeScan {
        let node = btree.getnode(pgno);

        if node.is_leaf {
            let keys: Vec<u64> = node.cells.iter().map(|(key, _)| integer_key(key)).collect();
            assert_sorted(&keys, context);
            if !is_root {
                assert!(
                    keys.len() >= btree.m,
                    "{context}: leaf page {pgno} has too few keys"
                );
            }
            assert!(
                keys.len() < 2 * btree.m,
                "{context}: leaf page {pgno} has too many keys"
            );
            return TreeScan {
                keys,
                leaves: vec![pgno],
            };
        }

        assert!(
            node.children_pgno.len() == node.cells.len()
                || node.children_pgno.len() == node.cells.len() + 1,
            "{context}: internal page {pgno} child/key count"
        );
        if !is_root {
            assert!(
                node.cells.len() >= btree.m,
                "{context}: internal page {pgno} has too few keys"
            );
        }
        assert!(
            node.cells.len() < 2 * btree.m,
            "{context}: internal page {pgno} has too many keys"
        );

        let mut all_keys = Vec::new();
        let mut leaves = Vec::new();
        for (idx, child_pgno) in node.children_pgno.iter().enumerate() {
            let child = scan_node(btree, *child_pgno, false, context);
            assert!(
                !child.keys.is_empty(),
                "{context}: internal page {pgno} points at empty child {child_pgno}"
            );
            if idx < node.cells.len() {
                assert_eq!(
                    integer_key(&node.cells[idx].0),
                    *child.keys.last().expect("child checked nonempty"),
                    "{context}: separator on page {pgno} index {idx}"
                );
            }
            all_keys.extend(child.keys);
            leaves.extend(child.leaves);
        }

        assert_sorted(&all_keys, context);
        TreeScan {
            keys: all_keys,
            leaves,
        }
    }

    fn assert_sorted(keys: &[u64], context: &str) {
        for pair in keys.windows(2) {
            assert!(pair[0] < pair[1], "{context}: keys are not sorted");
        }
    }

    fn integer_key(key: &BTreeDataType) -> u64 {
        match key {
            BTreeDataType::Integer(value) => *value,
            _ => core::panic!("expected integer key"),
        }
    }

    fn shuffled_keys(count: u64, seed: u64) -> Vec<u64> {
        let mut keys: Vec<u64> = (0..count).collect();
        let mut state = seed | 1;

        for i in (1..keys.len()).rev() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let j = (state as usize) % (i + 1);
            keys.swap(i, j);
        }

        keys
    }
}
