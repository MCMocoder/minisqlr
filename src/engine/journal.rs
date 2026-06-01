use std::{
    cell::Cell,
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek, Write},
    mem,
    path::Path,
};

#[cfg(target_os = "windows")]
use std::os::windows::io::AsRawHandle;

#[cfg(target_os = "windows")]
use winapi::um::{
    fileapi::{LockFileEx, UnlockFile},
    minwinbase::{LOCKFILE_EXCLUSIVE_LOCK, OVERLAPPED},
};

use super::pages::PAGE_SIZE;

pub const NONE_LOCK: u8 = 0;
pub const SHARED_LOCK: u8 = 1;
pub const RESERVED_LOCK: u8 = 2;
pub const PENDING_LOCK: u8 = 3;
pub const EXCLUSIVE_LOCK: u8 = 4;

pub trait FileLock {
    fn lock_shared(&self, start: usize, len: usize);
    fn lock_shared_all(&self);
    fn lock_exclusive(&self, start: usize, len: usize);
    fn lock_exclusive_all(&self);
    fn unlock(&self, start: usize, len: usize);
    fn unlock_all(&self);
}

#[cfg(target_os = "windows")]
impl FileLock for File {
    fn lock_shared(&self, start: usize, len: usize) {
        unsafe {
            let mut overlapped: OVERLAPPED = mem::zeroed();
            overlapped.u.s_mut().Offset = (start & ((!0u32) as usize)) as u32;
            overlapped.u.s_mut().OffsetHigh = (start >> 32usize) as u32;
            let lenlow = (len & ((!0u32) as usize)) as u32;
            let lenhigh = (len >> 32usize) as u32;
            let _ = LockFileEx(self.as_raw_handle(), 0, 0, lenlow, lenhigh, &mut overlapped);
        }
    }

    fn lock_shared_all(&self) {
        unsafe {
            let mut overlapped: OVERLAPPED = mem::zeroed();
            let _ = LockFileEx(self.as_raw_handle(), 0, 0, !0, !0, &mut overlapped);
        }
    }

    fn lock_exclusive(&self, start: usize, len: usize) {
        unsafe {
            let mut overlapped: OVERLAPPED = mem::zeroed();
            overlapped.u.s_mut().Offset = (start & ((!0u32) as usize)) as u32;
            overlapped.u.s_mut().OffsetHigh = (start >> 32usize) as u32;
            let lenlow = (len & ((!0u32) as usize)) as u32;
            let lenhigh = (len >> 32usize) as u32;
            let _ = LockFileEx(
                self.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                lenlow,
                lenhigh,
                &mut overlapped,
            );
        }
    }

    fn lock_exclusive_all(&self) {
        unsafe {
            let mut overlapped: OVERLAPPED = mem::zeroed();
            let _ = LockFileEx(
                self.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                !0,
                !0,
                &mut overlapped,
            );
        }
    }

    fn unlock(&self, start: usize, len: usize) {
        unsafe {
            let olow = (start & ((!0u32) as usize)) as u32;
            let ohigh = (start >> 32usize) as u32;
            let lenlow = (len & ((!0u32) as usize)) as u32;
            let lenhigh = (len >> 32usize) as u32;
            let _ = UnlockFile(self.as_raw_handle(), olow, ohigh, lenlow, lenhigh);
        }
    }

    fn unlock_all(&self) {
        unsafe {
            let _ = UnlockFile(self.as_raw_handle(), 0, 0, !0, !0);
        }
    }
}

pub struct Journal {
    db: File,
    dbname: String,
    journal: Option<File>,
    backup_set: HashSet<u32>,
    dblock: Cell<u8>,
    gotpending: Cell<bool>,
}

pub const PENDING_BYTE: usize = 0;
pub const RESERVED_BYTE: usize = 1;
pub const SHARED_BYTE: usize = 2;

pub const VALID_BYTE: usize = 16;
pub const JOURNAL_VALID_BYTE: usize = 16;
pub const JOURNAL_PAGE_COUNT: usize = 20;
pub const JOURNAL_HEADER: usize = 4096;

impl Journal {
    pub fn new(db: File, dbname: String) -> Self {
        Journal {
            db,
            dbname,
            journal: None,
            backup_set: HashSet::new(),
            dblock: Cell::new(NONE_LOCK),
            gotpending: Cell::new(false),
        }
    }

    pub fn readpage(&mut self, pgno: u32, buf: &mut [u8]) {
        let offset = pgno as usize * PAGE_SIZE;
        let _ = self.db.seek(std::io::SeekFrom::Start(offset as u64));
        self.db.read_exact(buf).expect("Read page error");
    }

    pub fn sync_db(&self) {
        let _ = self.db.sync_all();
    }

    pub fn lock_exclusive(&self) {
        if self.dblock.get() == EXCLUSIVE_LOCK {
            return;
        }

        FileLock::lock_exclusive(&self.db, PENDING_BYTE, 1);
        self.gotpending.set(true);

        if self.dblock.get() >= SHARED_LOCK {
            FileLock::unlock(&self.db, SHARED_BYTE, 1);
        }

        FileLock::lock_exclusive(&self.db, SHARED_BYTE, 1);
        self.dblock.set(EXCLUSIVE_LOCK);
    }

    pub fn begin_transaction_read(&self) {
        if self.dblock.get() >= SHARED_LOCK {
            return;
        }

        FileLock::lock_exclusive(&self.db, PENDING_BYTE, 1);
        self.gotpending.set(true);

        FileLock::lock_shared(&self.db, SHARED_BYTE, 1);
        FileLock::unlock(&self.db, PENDING_BYTE, 1);
        self.dblock.set(SHARED_LOCK);
        self.gotpending.set(false);
    }

    pub fn begin_rollback(&mut self) {
        if self.dblock.get() == RESERVED_LOCK || self.dblock.get() == EXCLUSIVE_LOCK {
            return;
        }

        FileLock::lock_exclusive(&self.db, RESERVED_BYTE, 1);
        self.dblock.set(RESERVED_LOCK);
    }

    pub fn begin_transaction_write(&mut self, pagecnt: u32) {
        if self.dblock.get() == RESERVED_LOCK || self.dblock.get() == EXCLUSIVE_LOCK {
            return;
        }

        FileLock::lock_exclusive(&self.db, RESERVED_BYTE, 1);
        self.dblock.set(RESERVED_LOCK);
        self.backup_set.clear();

        let binding = self.dbname.to_owned() + "-journal";
        let journal_path = Path::new(&binding);

        if journal_path.exists() {
            self.add_journal(journal_path);
            self.reset_journal(pagecnt);
            return;
        }

        self.journal = Some(
            File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(self.dbname.clone() + "-journal")
                .expect("Create journal error"),
        );

        match &mut self.journal {
            Some(j) => {
                let magic = b"MINSQL_JOURNAL";
                let mut headerpage = [0u8; JOURNAL_HEADER];
                headerpage[0..magic.len()].copy_from_slice(magic);
                headerpage[JOURNAL_VALID_BYTE] = 1u8;
                headerpage[JOURNAL_PAGE_COUNT..JOURNAL_PAGE_COUNT + 4]
                    .copy_from_slice(&pagecnt.to_be_bytes());
                let _ = j.write_all(&headerpage);
            }
            None => panic!("Create journal error"),
        }
    }

    pub fn commit_page(&mut self, pgno: u32, buf: &[u8]) {
        self.lock_exclusive();
        self.sync_journal();

        let offset = pgno as usize * PAGE_SIZE;
        let _ = self.db.seek(std::io::SeekFrom::Start(offset as u64));
        let _ = self.db.write_all(buf);
    }

    pub fn write_page(&mut self, pgno: u32, buf: &[u8]) {
        let offset = pgno as usize * PAGE_SIZE;
        let _ = self.db.seek(std::io::SeekFrom::Start(offset as u64));
        let _ = self.db.write_all(buf);
    }

    pub fn end_transaction_write(&mut self) {
        self.lock_exclusive();
    }

    pub fn backup_page(&mut self, pgno: u32, buf: &[u8]) {
        if self.backup_set.contains(&pgno) {
            return;
        }

        let checksum = buf.iter().fold(0u8, |checksum, byte| checksum ^ byte);

        match &mut self.journal {
            Some(j) => {
                let _ = j.write_all(&pgno.to_be_bytes());
                let _ = j.write_all(buf);
                let _ = j.write_all(&checksum.to_be_bytes());
            }
            None => panic!("Journal file is not open"),
        };

        self.backup_set.insert(pgno);
    }

    pub fn sync_journal(&mut self) {
        let _ = match &self.journal {
            Some(j) => j.sync_all(),
            None => panic!("Journal file is not open"),
        };
    }

    pub fn invalidate_journal(&mut self) {
        match &mut self.journal {
            Some(j) => {
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_VALID_BYTE as u64));
                let _ = j.write_all(&[0]);
                let _ = j.sync_all();
            }
            None => panic!("Journal file is not open"),
        };

        self.backup_set.clear();
    }

    pub fn revalidate_journal(&mut self) {
        match &mut self.journal {
            Some(j) => {
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_VALID_BYTE as u64));
                let _ = j.write_all(&[1]);
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_HEADER as u64));
                let _ = j.sync_all();
            }
            None => panic!("Journal file is not open"),
        };
    }

    pub fn reset_journal(&mut self, pagecnt: u32) {
        match &mut self.journal {
            Some(j) => {
                let _ = j.set_len(JOURNAL_HEADER as u64);
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_VALID_BYTE as u64));
                let _ = j.write_all(&[1]);
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_PAGE_COUNT as u64));
                let _ = j.write_all(&pagecnt.to_be_bytes());
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_HEADER as u64));
                let _ = j.sync_all();
            }
            None => panic!("Journal file is not open"),
        };
    }

    pub fn add_journal(&mut self, path: &Path) {
        self.journal = Some(
            File::options()
                .read(true)
                .write(true)
                .open(path)
                .expect("Open journal error"),
        );
    }

    pub fn is_journal_valid(&mut self) -> bool {
        match &mut self.journal {
            Some(j) => {
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_VALID_BYTE as u64));
                let mut buf = [0u8];
                let _ = j.read_exact(&mut buf);
                buf[0] != 0
            }
            None => panic!("Journal file is not open"),
        }
    }

    pub fn check_journal(&mut self) -> bool {
        const JOURNAL_PAGE: usize = PAGE_SIZE + 4 + 1;

        match &mut self.journal {
            Some(j) => {
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_HEADER as u64));
                let mut buf = [0u8; JOURNAL_PAGE];

                loop {
                    match j.read(&mut buf) {
                        Ok(0) => break true,
                        Ok(len) if len != JOURNAL_PAGE => break false,
                        Ok(_) => {
                            let content = &buf[4..4 + PAGE_SIZE];
                            let checksum =
                                content.iter().fold(0u8, |checksum, byte| checksum ^ byte);
                            let stored_checksum = buf[JOURNAL_PAGE - 1];

                            if stored_checksum != checksum {
                                break false;
                            }
                        }
                        Err(_) => panic!("Read journal error"),
                    }
                }
            }
            None => panic!("Journal file is not open"),
        }
    }

    pub fn unlock_write(&mut self) {
        FileLock::unlock(&self.db, SHARED_BYTE, 1);
        FileLock::unlock(&self.db, RESERVED_BYTE, 1);

        if self.gotpending.get() {
            FileLock::unlock(&self.db, PENDING_BYTE, 1);
        }

        self.dblock.set(NONE_LOCK);
        self.gotpending.set(false);
    }

    pub fn unlock_read(&mut self) {
        if self.dblock.get() >= SHARED_LOCK {
            FileLock::unlock(&self.db, SHARED_BYTE, 1);
        }

        self.dblock.set(NONE_LOCK);
    }

    pub fn rollback(&mut self) -> u32 {
        const JOURNAL_PAGE: usize = PAGE_SIZE + 4 + 1;

        match &mut self.journal {
            Some(j) => {
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_PAGE_COUNT as u64));
                let mut pagecnt_buf = [0u8; 4];
                let _ = j.read_exact(&mut pagecnt_buf);
                let pagecnt = u32::from_be_bytes(pagecnt_buf);
                let _ = j.seek(std::io::SeekFrom::Start(JOURNAL_HEADER as u64));
                let mut buf = [0u8; JOURNAL_PAGE];

                loop {
                    match j.read(&mut buf) {
                        Ok(0) => break,
                        Ok(len) if len != JOURNAL_PAGE => panic!("Incomplete journal page"),
                        Ok(_) => {
                            let mut pgno_buf = [0u8; 4];
                            pgno_buf.copy_from_slice(&buf[0..4]);
                            let pgno = u32::from_be_bytes(pgno_buf);
                            let content = &buf[4..4 + PAGE_SIZE];

                            let offset = pgno as usize * PAGE_SIZE;
                            let _ = self.db.seek(std::io::SeekFrom::Start(offset as u64));
                            let _ = self.db.write_all(content);
                        }
                        Err(e) => panic!("{}", e),
                    }
                }
                pagecnt
            }
            None => panic!("Journal file is not open"),
        }
    }

    pub fn fetch_db_pagecnt(&mut self) -> u32 {
        let len = fs::metadata(self.dbname.to_owned()).unwrap().len();
        (len as u32) / (PAGE_SIZE as u32)
    }

    pub fn shrink(&mut self, pagecnt: u32) {
        let _ = self.db.set_len(PAGE_SIZE as u64 * pagecnt as u64);
    }
}
