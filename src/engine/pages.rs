use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    io::{Seek, Write},
    ops::Deref,
    path::Path,
    rc::Rc,
};

use super::journal::{Journal, VALID_BYTE};

pub type PagePtr = Rc<RefCell<PageInfo>>;

pub const PAGE_SIZE: usize = 4096;

pub enum PagerState {
    Start,
    Read,
    Writebuf,
    Writefile,
}

pub struct Pager {
    dbname: String,
    state: Cell<PagerState>,
    journal: Rc<RefCell<Journal>>,
    lru: Rc<RefCell<VecDeque<PagePtr>>>,
    pinned_pages: Cell<u64>,
    initial_pages: Cell<u32>,
    total_pages: Cell<u32>,
    max_pinned: u64,
    max_pages: usize,
    pages_hash: Rc<RefCell<HashMap<u32, PagePtr>>>,
    modified: Rc<RefCell<HashSet<u32>>>,
}

impl Pager {
    pub fn new(db: File, dbname: String) -> Self {
        Pager {
            dbname: dbname.to_owned(),
            state: Cell::new(PagerState::Start),
            journal: Rc::new(RefCell::new(Journal::new(db, dbname.to_owned()))),
            lru: Rc::new(RefCell::new(VecDeque::new())),
            pinned_pages: Cell::new(0),
            initial_pages: Cell::new(0),
            total_pages: Cell::new(0),
            max_pinned: 100,
            max_pages: 10,
            pages_hash: Rc::new(RefCell::new(HashMap::new())),
            modified: Rc::new(RefCell::new(HashSet::new())),
        }
    }

    pub fn get_total_pages(&self) -> u32 {
        self.total_pages.get()
    }

    pub fn extend_page_count(&self, pagecnt: u32) {
        if pagecnt > self.total_pages.get() {
            self.total_pages.set(pagecnt);
        }
    }

    pub fn check_and_tryrollback(&self) {
        if self.is_journal_exists(self.dbname.to_owned()) {
            let journal_state = self.journal.borrow_mut().check_journal();
            if !journal_state {
                // 日志文件损坏，删除日志文件

                // 获取exclusive锁
                self.journal.borrow_mut().begin_rollback();
                self.journal.borrow_mut().lock_exclusive();

                self.journal.borrow_mut().revalidate_journal();
            } else {
                // 回滚
                self.rollback();
            }
        }
    }

    fn rollback(&self) {
        // 获取exclusive锁
        self.journal.borrow_mut().begin_rollback();
        self.journal.borrow_mut().lock_exclusive();

        // 回滚数据库文件并存盘
        let pagecnt = self.journal.borrow_mut().rollback();
        if pagecnt != 0 {
            self.journal.borrow_mut().shrink(pagecnt);
            self.total_pages.set(pagecnt);
            self.initial_pages.set(pagecnt);
        }
        self.journal.borrow_mut().sync_db();

        // 重新启用日志文件
        self.journal.borrow_mut().invalidate_journal();
        self.modified.borrow_mut().clear();
        self.initial_pages.set(self.total_pages.get());
        self.reload_cache_after_rollback();

        // 解除锁
        self.initial_pages.set(self.total_pages.get());
        self.journal.borrow_mut().unlock_write();
    }

    fn reload_cache_after_rollback(&self) {
        let pages: Vec<PagePtr> = self.pages_hash.borrow().values().cloned().collect();
        for p in pages {
            let pgno = p.borrow().pgno();
            if pgno < self.total_pages.get() {
                self.readto(&p, pgno);
                let mut page = p.borrow_mut();
                page.inner.flags &= !PAGE_EMPTY;
                self.mark_clean(&mut page);
            } else {
                let mut page = p.borrow_mut();
                page.inner.data.fill(0);
                page.inner.flags |= PAGE_EMPTY;
                self.mark_clean(&mut page);
            }
        }
    }

    fn is_journal_exists(&self, name: String) -> bool {
        let binding = name.to_owned() + "-journal";
        let journal_path = Path::new(&binding);
        if !journal_path.exists() {
            false
        } else {
            self.journal.borrow_mut().add_journal(journal_path);
            self.journal.borrow_mut().is_journal_valid()
        }
    }

    pub fn create_or_opendb(name: String) -> File {
        match File::create_new(name.to_owned()) {
            Ok(mut db) => {
                let str = b"MINSQL";
                let mut headerpage = [0u8; 4096];
                headerpage[0..str.len()].copy_from_slice(str);
                headerpage[VALID_BYTE] = 1;
                // 此处由于创建数据库，没必要获取锁
                let _ = db.seek(std::io::SeekFrom::Start(0));
                let _ = db.write(&headerpage);
                db
            }
            Err(_e) => File::options()
                .write(true)
                .truncate(false)
                .create(false)
                .read(true)
                .open(name.to_owned())
                .expect("Fuck"),
        }
    }

    fn page_pin(&self, page: &PagePtr) {
        if page.borrow().refcnt.get() == 0 {
            self.lru.borrow_mut().retain(|e| !Rc::ptr_eq(e, page));
            self.pinned_pages.set(self.pinned_pages.get() + 1);
        }
    }

    fn page_unpin(&self, page: &PagePtr) {
        self.lru.borrow_mut().push_front(page.clone());
        self.pinned_pages.set(self.pinned_pages.get() - 1);
    }

    fn alloc_page_and_read(&self, pgno: u32) -> PagePtr {
        let page = Rc::new(RefCell::new(PageInfo::new(
            pgno,
            0,
            Box::new([0u8; PAGE_SIZE]),
        )));
        self.pages_hash.borrow_mut().insert(pgno, page.clone());
        self.pinned_pages.set(self.pinned_pages.get() + 1);
        if pgno < self.total_pages.get() {
            // 数据库文件中存在该页面，需要从数据库文件加载页面data部分
            self.readto(&page, pgno);
        } else {
            // 数据库文件中不存在该页面
            let mut pr = page.borrow_mut();
            pr.inner.flags = pr.inner.flags | PAGE_EMPTY;
            drop(pr);
            self.mark_dirty(&mut page.borrow_mut());
        }
        page
    }

    pub fn acquire(&self, page: &PagePtr) {
        let inner = page.borrow_mut();
        inner.refcnt.set(inner.refcnt.get() + 1);
    }

    pub fn release(&self, page: &PagePtr) {
        let inner = page.borrow_mut();
        inner.refcnt.set(inner.refcnt.get() - 1);
        if inner.refcnt.get() == 0 {
            self.page_unpin(page);
        }
    }

    fn fetch_cache(&self, pgno: u32) -> Option<PagePtr> {
        // 该函数返回引用计数为0的页面
        if let Some(p) = self.pages_hash.borrow_mut().get(&pgno) {
            self.page_pin(p);
            return Some(p.clone()); // 页面存在于哈希表内
        }
        // 页面不存在于哈希表内
        if self.pages_hash.borrow_mut().len() >= self.max_pages {
            // 内存紧张，遇到软内存限制
            if self.pinned_pages.get() >= self.max_pinned {
                None // 被Pin住的页面超出限制
            } else {
                // 页面可以被Pin住，从lru队列中回收页面，此时待回收的页面存在于哈希表内，需要rekey之后再加入哈希表，
                match self.lru.borrow_mut().pop_back() {
                    Some(p) => {
                        // 页面刷盘 此时会获得exclusive锁
                        let mut page = p.borrow_mut();
                        if page.inner.dirty {
                            self.commit_page(&mut page);
                        }
                        self.mark_clean(&mut page);
                        drop(page);
                        // 更改页面编号
                        let oldno = p.borrow_mut().fetchkey_rekey(pgno);
                        self.pages_hash.borrow_mut().remove(&oldno);
                        self.pages_hash.borrow_mut().insert(pgno, p.clone());
                        if pgno < self.total_pages.get() {
                            p.borrow_mut().inner.flags &= !PAGE_EMPTY;
                            // 数据库文件中存在该页面，需要从数据库文件加载页面data部分
                            self.readto(&p, pgno);
                        } else {
                            // 数据库文件中不存在该页面
                            let mut pr = p.borrow_mut();
                            pr.inner.flags = pr.inner.flags | PAGE_EMPTY;
                            drop(pr);
                            self.mark_dirty(&mut p.borrow_mut());
                        }
                        // 此处页面已经被pin住，不需要重复执行page_pin()
                        self.pinned_pages.set(self.pinned_pages.get() + 1);
                        Some(p)
                    }
                    None => {
                        if self.pinned_pages.get() < self.max_pinned {
                            Some(self.alloc_page_and_read(pgno))
                        } else {
                            None
                        }
                    }
                }
            }
        } else {
            // 哈希表未满，系统内存充足，分配新页面
            Some(self.alloc_page_and_read(pgno))
        }
    }

    fn fetch_inner(&self, pgno: u32) -> Option<PagePtr> {
        match self.fetch_cache(pgno) {
            Some(p) => {
                self.acquire(&p);
                Some(p)
            }
            None => None,
        }
    }

    fn mark_dirty(&self, page: &mut PageInfo) {
        page.inner.dirty = true;
    }

    fn mark_clean(&self, page: &mut PageInfo) {
        page.inner.dirty = false;
    }

    fn commit_page(&self, page: &mut PageInfo) {
        self.state.set(PagerState::Writefile);
        self.journal
            .borrow_mut()
            .commit_page(page.inner.pgno, &page.inner.data);
    }

    pub fn fetch_ref(&self, pgno: u32) -> Option<PageRef<'_>> {
        match self.fetch_inner(pgno) {
            Some(p) => Some(PageRef::new(self, &p)),
            None => None,
        }
    }

    pub fn fetch(&self, pgno: u32) -> Option<PagePtr> {
        self.fetch_inner(pgno)
    }

    fn readto(&self, page: &PagePtr, pgno: u32) {
        // Note:该函数会从磁盘中读入文件的一部分,调用此函数需要保证文件已经获得shared或者更高级别的锁
        let mut page = page.borrow_mut();
        self.journal
            .borrow_mut()
            .readpage(pgno, &mut page.inner.data);
    }

    pub fn pagecnt(&self) -> usize {
        self.pages_hash.borrow_mut().len()
    }

    pub fn end_transaction_write(&self) {
        // 事务结束落盘
        // 获取exclusive锁，准备写数据库文件
        self.journal.borrow_mut().end_transaction_write();
        self.state.set(PagerState::Writefile);

        // 日志落盘
        self.journal.borrow_mut().sync_journal();

        // 缓存中的脏页面落盘
        self.lru.borrow_mut().clear();
        let pages_hash = self.pages_hash.borrow_mut();
        for (_pgno, p) in pages_hash.iter() {
            // 提交页面
            let dirty = p.borrow_mut().inner.dirty;
            if dirty {
                // 脏页面落盘
                let page = p.borrow_mut();
                let mut page = page;
                self.journal
                    .borrow_mut()
                    .write_page(page.inner.pgno, &page.inner.data);
                self.mark_clean(&mut page);
            }
        }
        // 同步数据库文件
        self.journal.borrow_mut().sync_db();

        // 使日志文件失效
        self.journal.borrow_mut().invalidate_journal();
        self.modified.borrow_mut().clear();

        // 解除所有锁
        self.initial_pages.set(self.total_pages.get());
        self.journal.borrow_mut().unlock_write();
    }

    pub fn end_transaction_read(&self) {
        self.journal.borrow_mut().unlock_read();
    }

    pub fn begin_transaction_read(&self) {
        self.journal.borrow_mut().begin_transaction_read();
        self.state.set(PagerState::Read);

        // 检查数据库文件完整性
        self.check_and_tryrollback();

        // 获取数据库文件元数据
        let pagecnt = self.journal.borrow_mut().fetch_db_pagecnt();
        self.initial_pages.set(pagecnt);
        self.total_pages.set(pagecnt);
    }

    pub fn begin_transaction_write(&self) {
        self.journal
            .borrow_mut()
            .begin_transaction_write(self.total_pages.get());
        self.state.set(PagerState::Writebuf);
    }

    pub fn will_modify(&self, page: &PagePtr) {
        // 已经获取reserved锁，可以直接备份
        let mut page = page.borrow_mut();
        if page.inner.pgno < self.initial_pages.get()
            && !self.modified.borrow().contains(&page.inner.pgno)
        {
            self.modified.borrow_mut().insert(page.inner.pgno);
            self.journal
                .borrow_mut()
                .backup_page(page.inner.pgno, &page.inner.data);
        }
        self.mark_dirty(&mut page);
    }

    pub fn mark_inited(&self, page: &PagePtr) {
        let mut page = page.borrow_mut();
        page.inner.flags = page.inner.flags & (!PAGE_EMPTY);
    }

    pub fn shrink_file(&self, pagecnt: u32) {
        self.journal.borrow_mut().shrink(pagecnt);
    }
}

pub struct PageInfo {
    pub refcnt: Cell<u64>,
    inner: PageInfoInner,
}

impl PageInfo {
    fn new(pgno: u32, flags: u32, data: Box<[u8]>) -> Self {
        PageInfo {
            refcnt: Cell::new(0),
            inner: PageInfoInner {
                dirty: false,
                pgno,
                flags,
                data,
            },
        }
    }

    fn fetchkey_rekey(&mut self, pgno: u32) -> u32 {
        let old_no = self.inner.pgno;
        self.inner.pgno = pgno;
        old_no
    }

    pub fn data_write(&mut self) -> &mut [u8] {
        &mut self.inner.data
    }

    pub fn data_read(&mut self) -> &[u8] {
        &self.inner.data
    }

    pub fn pgno(&self) -> u32 {
        self.inner.pgno
    }

    pub fn flags(&self) -> u32 {
        self.inner.flags
    }
}

pub struct PageRef<'a> {
    pub pager: &'a Pager,
    pub info: PagePtr,
}

impl Deref for PageRef<'_> {
    type Target = PagePtr;

    fn deref(&self) -> &Self::Target {
        &self.info
    }
}

impl Drop for PageRef<'_> {
    fn drop(&mut self) {
        self.pager.release(&self.info);
    }
}

impl<'a> PageRef<'a> {
    fn new(pager: &'a Pager, page: &PagePtr) -> Self {
        PageRef {
            pager,
            info: page.clone(),
        }
    }
}

impl Clone for PageRef<'_> {
    fn clone(&self) -> Self {
        self.pager.acquire(&self.info);
        Self {
            pager: self.pager,
            info: self.info.clone(),
        }
    }
}

pub const PAGE_EMPTY: u32 = 1;

pub struct PageInfoInner {
    dirty: bool,
    pgno: u32,
    flags: u32,
    data: Box<[u8]>,
}
