//! P0 VFS: one main file/connection per registration; private native DELETE journal.
//! This crate never publishes remote objects. All FFI callbacks contain panics.
use remote_pager::{CowFile, FrozenPages};
use rusqlite::{ffi::*, Connection, OpenFlags};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tempfile::TempDir;
use thiserror::Error;
mod local;
pub use local::WorkspaceLimits;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Pager(#[from] remote_pager::Error),
    #[error("VFS initialization or lifecycle error")]
    Vfs,
}
struct State {
    parent: *mut sqlite3_vfs,
    main: CString,
    journal: CString,
    pager: Arc<Mutex<CowFile>>,
    opened: bool,
    local: Arc<Mutex<local::Budget>>,
}
#[repr(C)]
struct MainFile {
    base: sqlite3_file,
    pager: *mut Arc<Mutex<CowFile>>,
    lock: c_int,
}

/// Owns the connection, registration, callback state and private directory in that order.
/// No connection or statement may outlive this owner. Registration is never the default.
pub struct Workspace {
    connection: Option<Connection>,
    vfs: Box<sqlite3_vfs>,
    state: Box<State>,
    _name: CString,
    _directory: TempDir,
}
impl Workspace {
    pub fn open(pager: CowFile) -> Result<Self, Error> {
        Self::open_with_limits(pager, WorkspaceLimits::default())
    }
    pub fn open_with_limits(pager: CowFile, limits: WorkspaceLimits) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let directory = tempfile::tempdir()?;
        // Native journal creation reads the main path permissions. This empty
        // sentinel is never used for database bytes or snapshot recovery.
        std::fs::File::create(directory.path().join("main.db"))?;
        let main = CString::new(
            directory
                .path()
                .join("main.db")
                .to_str()
                .ok_or(Error::Vfs)?,
        )
        .map_err(|_| Error::Vfs)?;
        let journal = CString::new(format!(
            "{}-journal",
            main.to_str().map_err(|_| Error::Vfs)?
        ))
        .map_err(|_| Error::Vfs)?;
        let name = CString::new(format!(
            "ponydb-p0-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        // SAFETY: SQLite is initialized before looking up the process's native VFS.
        let parent = unsafe {
            sqlite3_initialize();
            sqlite3_vfs_find(ptr::null())
        };
        if parent.is_null() {
            return Err(Error::Vfs);
        }
        let mut state = Box::new(State {
            parent,
            main,
            journal,
            pager: Arc::new(Mutex::new(pager)),
            opened: false,
            local: Arc::new(Mutex::new(local::Budget::new(
                directory.path().to_path_buf(),
                limits,
            ))),
        });
        // SAFETY: zero is a valid initial representation for this C struct of numbers/pointers.
        let mut vfs: Box<sqlite3_vfs> = Box::new(unsafe { std::mem::zeroed() });
        vfs.iVersion = 1;
        vfs.szOsFile = unsafe { (*parent).szOsFile }.max(
            std::mem::size_of::<MainFile>().max(std::mem::size_of::<local::LocalFile>()) as c_int,
        );
        vfs.mxPathname = unsafe { (*parent).mxPathname };
        vfs.zName = name.as_ptr();
        vfs.pAppData = (&mut *state as *mut State).cast();
        vfs.xOpen = Some(open);
        vfs.xDelete = Some(delete);
        vfs.xAccess = Some(access);
        vfs.xFullPathname = Some(full_path);
        vfs.xRandomness = Some(randomness);
        vfs.xSleep = Some(sleep);
        vfs.xCurrentTime = Some(current_time);
        // Dynamic extension loading and shared-memory/WAL interfaces deliberately absent.
        if unsafe { sqlite3_vfs_register(&mut *vfs, 0) } != SQLITE_OK {
            return Err(Error::Vfs);
        }
        let mut owner = Self {
            connection: None,
            vfs,
            state,
            _name: name,
            _directory: directory,
        };
        owner.connection = Some(Connection::open_with_flags_and_vfs(
            owner.state.main.to_str().map_err(|_| Error::Vfs)?,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            owner._name.to_str().map_err(|_| Error::Vfs)?,
        )?);
        Ok(owner)
    }
    /// Internal engine access; callers must enforce the service SQL policy.
    pub fn connection(&self) -> &Connection {
        self.connection.as_ref().expect("live workspace")
    }
    /// Internal engine access for installing SQL policy and cancellation hooks.
    pub fn connection_mut(&mut self) -> &mut Connection {
        self.connection.as_mut().expect("live workspace")
    }
    /// Consumes the workspace. Caller must have successfully finished local COMMIT.
    pub fn freeze(mut self) -> Result<FrozenPages, Error> {
        if !self.connection().is_autocommit() {
            return Err(Error::Vfs);
        }
        let pages = self.state.pager.lock().map_err(|_| Error::Vfs)?.freeze()?;
        if let Some(c) = self.connection.take() {
            c.close().map_err(|(_, e)| Error::Sql(e))?;
        }
        Ok(pages)
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        drop(self.connection.take());
        // SAFETY: the connection is closed; boxes and name remain live through unregister.
        unsafe {
            sqlite3_vfs_unregister(&mut *self.vfs);
        }
    }
}
fn guard(f: impl FnOnce() -> c_int) -> c_int {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(SQLITE_IOERR)
}
// SAFETY for callbacks below: SQLite supplies pointers with the documented C ABI and
// buffer lengths. Registration owns State; xOpen installs MainFile only after success.
unsafe fn state<'a>(v: *mut sqlite3_vfs) -> &'a mut State {
    &mut *((*v).pAppData as *mut State)
}
unsafe fn is_name(name: *const c_char, expected: &CStr) -> bool {
    !name.is_null() && CStr::from_ptr(name) == expected
}
unsafe extern "C" fn open(
    v: *mut sqlite3_vfs,
    name: *const c_char,
    file: *mut sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    guard(|| {
        (*file).pMethods = ptr::null();
        let s = state(v);
        if flags & SQLITE_OPEN_MAIN_DB != 0 {
            if s.opened || !is_name(name, &s.main) {
                return SQLITE_CANTOPEN;
            }
            let f = file.cast::<MainFile>();
            ptr::write(
                f,
                MainFile {
                    base: sqlite3_file { pMethods: &METHODS },
                    pager: Box::into_raw(Box::new(s.pager.clone())),
                    lock: SQLITE_LOCK_NONE,
                },
            );
            s.opened = true;
            if !out.is_null() {
                *out = flags;
            }
            SQLITE_OK
        } else {
            local::open_local(s, name, file, flags, out)
        }
    })
}
unsafe extern "C" fn delete(v: *mut sqlite3_vfs, name: *const c_char, sync: c_int) -> c_int {
    guard(|| {
        let s = state(v);
        if !is_name(name, &s.journal) {
            return SQLITE_IOERR_DELETE;
        }
        match s.local.lock() {
            Ok(mut b) => b.delete(
                std::path::PathBuf::from(s.journal.to_str().unwrap()),
                sync != 0,
            ),
            Err(_) => SQLITE_IOERR_DELETE,
        }
    })
}
unsafe extern "C" fn access(
    v: *mut sqlite3_vfs,
    name: *const c_char,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    guard(|| {
        let s = state(v);
        if is_name(name, &s.main) {
            *out = 1;
            SQLITE_OK
        } else if is_name(name, &s.journal) {
            let _ = flags;
            match std::fs::metadata(s.journal.to_str().unwrap()) {
                Ok(_) => {
                    *out = 1;
                    SQLITE_OK
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    *out = 0;
                    SQLITE_OK
                }
                Err(_) => SQLITE_IOERR_ACCESS,
            }
        } else {
            *out = 0;
            SQLITE_OK
        }
    })
}
unsafe extern "C" fn full_path(
    v: *mut sqlite3_vfs,
    name: *const c_char,
    n: c_int,
    out: *mut c_char,
) -> c_int {
    guard(|| {
        let s = state(v);
        if !is_name(name, &s.main) {
            return SQLITE_CANTOPEN;
        }
        let bytes = s.main.as_bytes_with_nul();
        if n < 0 || bytes.len() > n as usize {
            return SQLITE_CANTOPEN;
        }
        ptr::copy_nonoverlapping(bytes.as_ptr().cast(), out, bytes.len());
        SQLITE_OK
    })
}
unsafe extern "C" fn randomness(v: *mut sqlite3_vfs, n: c_int, out: *mut c_char) -> c_int {
    guard(|| {
        let s = state(v);
        ((*s.parent).xRandomness.unwrap())(s.parent, n, out)
    })
}
unsafe extern "C" fn sleep(v: *mut sqlite3_vfs, n: c_int) -> c_int {
    guard(|| {
        let s = state(v);
        ((*s.parent).xSleep.unwrap())(s.parent, n)
    })
}
unsafe extern "C" fn current_time(v: *mut sqlite3_vfs, out: *mut f64) -> c_int {
    guard(|| {
        let s = state(v);
        ((*s.parent).xCurrentTime.unwrap())(s.parent, out)
    })
}
unsafe fn with_pager(file: *mut sqlite3_file, f: impl FnOnce(&mut CowFile) -> c_int) -> c_int {
    guard(|| {
        let p = &*(*file.cast::<MainFile>()).pager;
        match p.lock() {
            Ok(mut p) => f(&mut p),
            Err(_) => SQLITE_IOERR,
        }
    })
}
unsafe extern "C" fn close(file: *mut sqlite3_file) -> c_int {
    guard(|| {
        let f = &mut *file.cast::<MainFile>();
        drop(Box::from_raw(f.pager));
        f.pager = ptr::null_mut();
        f.base.pMethods = ptr::null();
        SQLITE_OK
    })
}
unsafe extern "C" fn read(
    file: *mut sqlite3_file,
    out: *mut c_void,
    n: c_int,
    offset: i64,
) -> c_int {
    if n < 0 || offset < 0 {
        return SQLITE_IOERR_READ;
    }
    with_pager(file, |p| {
        match p.read(
            offset as u64,
            std::slice::from_raw_parts_mut(out.cast(), n as usize),
        ) {
            Ok(true) => SQLITE_OK,
            Ok(false) => SQLITE_IOERR_SHORT_READ,
            Err(_) => SQLITE_IOERR_READ,
        }
    })
}
unsafe extern "C" fn write(
    file: *mut sqlite3_file,
    data: *const c_void,
    n: c_int,
    offset: i64,
) -> c_int {
    if n < 0 || offset < 0 {
        return SQLITE_IOERR_WRITE;
    }
    with_pager(file, |p| {
        match p.write(
            offset as u64,
            std::slice::from_raw_parts(data.cast(), n as usize),
        ) {
            Ok(()) => SQLITE_OK,
            Err(remote_pager::Error::Budget) => SQLITE_FULL,
            Err(_) => SQLITE_IOERR_WRITE,
        }
    })
}
unsafe extern "C" fn truncate(file: *mut sqlite3_file, size: i64) -> c_int {
    if size < 0 {
        return SQLITE_IOERR_TRUNCATE;
    }
    with_pager(file, |p| match p.truncate(size as u64) {
        Ok(()) => SQLITE_OK,
        Err(_) => SQLITE_IOERR_TRUNCATE,
    })
}
unsafe extern "C" fn sync(_: *mut sqlite3_file, _: c_int) -> c_int {
    SQLITE_OK
} // volatile candidate only
unsafe extern "C" fn file_size(file: *mut sqlite3_file, out: *mut i64) -> c_int {
    with_pager(file, |p| {
        *out = p.len() as i64;
        SQLITE_OK
    })
}
unsafe extern "C" fn lock(file: *mut sqlite3_file, level: c_int) -> c_int {
    guard(|| {
        let f = &mut *file.cast::<MainFile>();
        f.lock = f.lock.max(level);
        SQLITE_OK
    })
}
unsafe extern "C" fn unlock(file: *mut sqlite3_file, level: c_int) -> c_int {
    guard(|| {
        (*file.cast::<MainFile>()).lock = level;
        SQLITE_OK
    })
}
unsafe extern "C" fn reserved(file: *mut sqlite3_file, out: *mut c_int) -> c_int {
    guard(|| {
        *out = i32::from((*file.cast::<MainFile>()).lock >= SQLITE_LOCK_RESERVED);
        SQLITE_OK
    })
}
unsafe extern "C" fn control(_: *mut sqlite3_file, _: c_int, _: *mut c_void) -> c_int {
    SQLITE_NOTFOUND
}
unsafe extern "C" fn sector(_: *mut sqlite3_file) -> c_int {
    512
}
unsafe extern "C" fn characteristics(_: *mut sqlite3_file) -> c_int {
    0
}
static METHODS: sqlite3_io_methods = sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(file_size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(reserved),
    xFileControl: Some(control),
    xSectorSize: Some(sector),
    xDeviceCharacteristics: Some(characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

#[cfg(test)]
mod tests {
    use super::*;
    struct Empty;
    impl remote_pager::Snapshot for Empty {
        fn len(&self) -> u64 {
            0
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> remote_pager::Result<()> {
            unreachable!()
        }
    }
    #[test]
    fn local_callbacks_quota_eof_truncation_deletion_and_lock_contract() {
        let mut ws = Workspace::open_with_limits(
            CowFile::new(Arc::new(Empty), 4096, 8192).unwrap(),
            WorkspaceLimits {
                max_local_bytes: 1024,
                max_local_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
        // Aligned backing allocation sized by the registered VFS ABI.
        let mut words =
            vec![0usize; (ws.vfs.szOsFile as usize).div_ceil(std::mem::size_of::<usize>())];
        let f = words.as_mut_ptr().cast::<sqlite3_file>();
        let name = ws.state.journal.clone();
        unsafe {
            assert_eq!(
                open(
                    &mut *ws.vfs,
                    name.as_ptr(),
                    f,
                    SQLITE_OPEN_MAIN_JOURNAL | SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
                    ptr::null_mut()
                ),
                SQLITE_OK
            );
            let m = &*(*f).pMethods;
            let data = vec![1u8; 1024];
            assert_eq!(
                m.xWrite.unwrap()(f, data.as_ptr().cast(), 1024, 0),
                SQLITE_OK
            );
            assert_eq!(
                m.xWrite.unwrap()(f, data.as_ptr().cast(), 1, 1024),
                SQLITE_FULL
            );
            let mut size = 0;
            assert_eq!(m.xFileSize.unwrap()(f, &mut size), SQLITE_OK);
            assert_eq!(size, 1024);
            assert_eq!(m.xTruncate.unwrap()(f, 8), SQLITE_OK);
            assert_eq!(m.xTruncate.unwrap()(f, 16), SQLITE_OK);
            let mut read = [9u8; 20];
            assert_eq!(
                m.xRead.unwrap()(f, read.as_mut_ptr().cast(), 20, 0),
                SQLITE_IOERR_SHORT_READ
            );
            assert_eq!(&read[..8], &[1; 8]);
            assert_eq!(&read[8..], &[0; 12]);
            assert_eq!(m.xSync.unwrap()(f, SQLITE_SYNC_FULL), SQLITE_OK);
            assert_eq!(
                m.xFileControl.unwrap()(f, 9999, ptr::null_mut()),
                SQLITE_NOTFOUND
            );
            assert_eq!(m.xDeviceCharacteristics.unwrap()(f), 0);
            assert_eq!(m.xClose.unwrap()(f), SQLITE_OK);
            let mut exists = 0;
            assert_eq!(
                access(
                    &mut *ws.vfs,
                    name.as_ptr(),
                    SQLITE_ACCESS_EXISTS,
                    &mut exists
                ),
                SQLITE_OK
            );
            assert_eq!(exists, 1);
            assert_eq!(delete(&mut *ws.vfs, name.as_ptr(), 1), SQLITE_OK);
            assert_eq!(
                access(
                    &mut *ws.vfs,
                    name.as_ptr(),
                    SQLITE_ACCESS_EXISTS,
                    &mut exists
                ),
                SQLITE_OK
            );
            assert_eq!(exists, 0);
            assert_eq!(
                open(
                    &mut *ws.vfs,
                    ptr::null(),
                    f,
                    SQLITE_OPEN_SUBJOURNAL
                        | SQLITE_OPEN_DELETEONCLOSE
                        | SQLITE_OPEN_READWRITE
                        | SQLITE_OPEN_CREATE,
                    ptr::null_mut()
                ),
                SQLITE_OK
            );
            let path = ws._directory.path().join("temp-1");
            assert!(path.exists());
            assert_eq!(((*(*f).pMethods).xClose.unwrap())(f), SQLITE_OK);
            assert!(!path.exists());
        }
    }
}
