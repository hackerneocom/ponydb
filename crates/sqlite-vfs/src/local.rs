//! Private OS files with a shared disk and descriptor budget. No caller paths.
use super::*;
use request_context::RequestContext;
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

#[derive(Clone, Debug)]
pub struct WorkspaceLimits {
    pub max_local_bytes: u64,
    pub max_local_files: usize,
    pub context: RequestContext,
}
impl Default for WorkspaceLimits {
    fn default() -> Self {
        Self {
            max_local_bytes: 64 * 1024 * 1024,
            max_local_files: 32,
            context: RequestContext::new(std::time::Duration::from_secs(30)),
        }
    }
}
pub(super) struct Budget {
    pub root: PathBuf,
    pub limits: WorkspaceLimits,
    files: HashMap<PathBuf, u64>,
    next: u64,
}
impl Budget {
    pub fn new(root: PathBuf, limits: WorkspaceLimits) -> Self {
        Self {
            root,
            limits,
            files: HashMap::new(),
            next: 0,
        }
    }
    fn resize(&mut self, path: &PathBuf, size: u64) -> bool {
        let old = self.files.get(path).copied().unwrap_or(0);
        let used: u64 = self.files.values().sum();
        if size > self.limits.max_local_bytes.saturating_sub(used - old) {
            return false;
        }
        self.files.insert(path.clone(), size);
        true
    }
    pub fn delete(&mut self, path: PathBuf, sync: bool) -> c_int {
        if self.limits.context.check().is_err() {
            return SQLITE_IOERR_DELETE;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return SQLITE_IOERR_DELETE,
        }
        self.files.remove(&path);
        #[cfg(unix)]
        if sync && File::open(&self.root).and_then(|f| f.sync_all()).is_err() {
            return SQLITE_IOERR_DIR_FSYNC;
        }
        #[cfg(not(unix))]
        let _ = sync;
        SQLITE_OK
    }
}
struct Local {
    file: File,
    path: PathBuf,
    delete_on_close: bool,
    budget: Arc<Mutex<Budget>>,
}
#[repr(C)]
pub(super) struct LocalFile {
    base: sqlite3_file,
    inner: *mut Local,
}

pub(super) unsafe fn open_local(
    s: &mut State,
    name: *const c_char,
    file: *mut sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    let Ok(mut b) = s.local.lock() else {
        return SQLITE_IOERR;
    };
    if b.limits.context.check().is_err() {
        return SQLITE_INTERRUPT;
    }
    if b.files.len() >= b.limits.max_local_files {
        return SQLITE_FULL;
    }
    let is_journal = flags & SQLITE_OPEN_MAIN_JOURNAL != 0 && is_name(name, &s.journal);
    let is_temp = name.is_null()
        && flags
            & (SQLITE_OPEN_TEMP_DB
                | SQLITE_OPEN_TEMP_JOURNAL
                | SQLITE_OPEN_SUBJOURNAL
                | SQLITE_OPEN_TRANSIENT_DB)
            != 0;
    if !is_journal && !is_temp {
        return SQLITE_CANTOPEN;
    }
    let path = if is_journal {
        PathBuf::from(s.journal.to_str().unwrap())
    } else {
        b.next += 1;
        b.root.join(format!("temp-{}", b.next))
    };
    let result = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path);
    let f = match result {
        Ok(f) => f,
        Err(_) => return SQLITE_CANTOPEN,
    };
    b.files.insert(path.clone(), 0);
    drop(b);
    let inner = Box::new(Local {
        file: f,
        path,
        delete_on_close: !is_journal,
        budget: s.local.clone(),
    });
    ptr::write(
        file.cast::<LocalFile>(),
        LocalFile {
            base: sqlite3_file {
                pMethods: &LOCAL_METHODS,
            },
            inner: Box::into_raw(inner),
        },
    );
    if !out.is_null() {
        *out = flags;
    }
    SQLITE_OK
}
unsafe fn with_local(file: *mut sqlite3_file, f: impl FnOnce(&mut Local) -> c_int) -> c_int {
    guard(|| {
        let local = &mut *(*file.cast::<LocalFile>()).inner;
        if local
            .budget
            .lock()
            .map(|b| b.limits.context.check().is_err())
            .unwrap_or(true)
        {
            return SQLITE_IOERR;
        }
        f(local)
    })
}
unsafe extern "C" fn local_close(file: *mut sqlite3_file) -> c_int {
    guard(|| {
        let f = &mut *file.cast::<LocalFile>();
        let local = Box::from_raw(f.inner);
        f.inner = ptr::null_mut();
        f.base.pMethods = ptr::null();
        let Local {
            file: os_file,
            path,
            delete_on_close,
            budget,
        } = *local;
        drop(os_file);
        if delete_on_close {
            // Cleanup must work even after the request is cancelled.
            if std::fs::remove_file(&path).is_err() {
                return SQLITE_IOERR_DELETE;
            }
            if let Ok(mut b) = budget.lock() {
                b.files.remove(&path);
            }
        }
        SQLITE_OK
    })
}
unsafe extern "C" fn local_read(
    file: *mut sqlite3_file,
    out: *mut c_void,
    n: c_int,
    offset: i64,
) -> c_int {
    if n < 0 || offset < 0 {
        return SQLITE_IOERR_READ;
    }
    with_local(file, |l| {
        let buf = std::slice::from_raw_parts_mut(out.cast::<u8>(), n as usize);
        buf.fill(0);
        if l.file.seek(SeekFrom::Start(offset as u64)).is_err() {
            return SQLITE_IOERR_SEEK;
        }
        let mut done = 0;
        while done < buf.len() {
            match l.file.read(&mut buf[done..]) {
                Ok(0) => return SQLITE_IOERR_SHORT_READ,
                Ok(n) => done += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return SQLITE_IOERR_READ,
            }
        }
        SQLITE_OK
    })
}
unsafe extern "C" fn local_write(
    file: *mut sqlite3_file,
    data: *const c_void,
    n: c_int,
    offset: i64,
) -> c_int {
    if n < 0 || offset < 0 {
        return SQLITE_IOERR_WRITE;
    }
    with_local(file, |l| {
        let Some(end) = (offset as u64)
            .checked_add(n as u64)
            .filter(|v| *v <= i64::MAX as u64)
        else {
            return SQLITE_FULL;
        };
        let Ok(mut b) = l.budget.lock() else {
            return SQLITE_IOERR;
        };
        let size = end.max(b.files.get(&l.path).copied().unwrap_or(0));
        if !b.resize(&l.path, size) {
            return SQLITE_FULL;
        }
        drop(b);
        if l.file
            .seek(SeekFrom::Start(offset as u64))
            .and_then(|_| {
                l.file
                    .write_all(std::slice::from_raw_parts(data.cast::<u8>(), n as usize))
            })
            .is_err()
        {
            return SQLITE_IOERR_WRITE;
        }
        SQLITE_OK
    })
}
unsafe extern "C" fn local_truncate(file: *mut sqlite3_file, size: i64) -> c_int {
    if size < 0 {
        return SQLITE_IOERR_TRUNCATE;
    }
    with_local(file, |l| {
        let Ok(mut b) = l.budget.lock() else {
            return SQLITE_IOERR;
        };
        // Reserve growth before touching the file; failed shrink keeps conservative accounting.
        let old = b.files.get(&l.path).copied().unwrap_or(0);
        if !b.resize(&l.path, old.max(size as u64)) {
            return SQLITE_FULL;
        }
        if l.file.set_len(size as u64).is_err() {
            return SQLITE_IOERR_TRUNCATE;
        }
        b.resize(&l.path, size as u64);
        SQLITE_OK
    })
}
unsafe extern "C" fn local_sync(file: *mut sqlite3_file, _: c_int) -> c_int {
    with_local(file, |l| {
        if l.file.sync_all().is_ok() {
            SQLITE_OK
        } else {
            SQLITE_IOERR_FSYNC
        }
    })
}
unsafe extern "C" fn local_size(file: *mut sqlite3_file, out: *mut i64) -> c_int {
    with_local(file, |l| match l.file.metadata() {
        Ok(m) => {
            *out = m.len() as i64;
            SQLITE_OK
        }
        Err(_) => SQLITE_IOERR_FSTAT,
    })
}
unsafe extern "C" fn local_lock(_: *mut sqlite3_file, _: c_int) -> c_int {
    SQLITE_OK
}
unsafe extern "C" fn local_reserved(_: *mut sqlite3_file, out: *mut c_int) -> c_int {
    *out = 0;
    SQLITE_OK
}
static LOCAL_METHODS: sqlite3_io_methods = sqlite3_io_methods {
    xClose: Some(local_close),
    xRead: Some(local_read),
    xWrite: Some(local_write),
    xTruncate: Some(local_truncate),
    xSync: Some(local_sync),
    xFileSize: Some(local_size),
    xLock: Some(local_lock),
    xUnlock: Some(local_lock),
    xCheckReservedLock: Some(local_reserved),
    ..METHODS
};
