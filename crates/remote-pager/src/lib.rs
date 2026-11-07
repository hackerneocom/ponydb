//! A candidate's private byte-addressable COW file over an immutable snapshot.
//! Pages are zero-based internally; freeze returns full page after-images.
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("snapshot I/O: {0}")]
    Io(String),
    #[error("invalid page size, range, or snapshot length")]
    Invalid,
    #[error("candidate dirty-page budget exceeded")]
    Budget,
    #[error("candidate is frozen")]
    Frozen,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Implementations bind their identity/version for their entire lifetime.
/// Return exactly the requested bytes, or an error (missing objects are not EOF).
pub trait Snapshot: Send + Sync {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenPages {
    pub page_size: usize,
    pub final_file_size: u64,
    pub pages: BTreeMap<u64, Vec<u8>>,
}

pub struct CowFile {
    base: Arc<dyn Snapshot>,
    page_size: usize,
    len: u64,
    // The base bytes beyond this watermark can never reappear after truncation.
    base_limit: u64,
    pages: BTreeMap<u64, Vec<u8>>,
    max_pages: usize,
    frozen: bool,
}
impl CowFile {
    pub fn new(base: Arc<dyn Snapshot>, page_size: usize, max_dirty_bytes: usize) -> Result<Self> {
        if !(512..=65536).contains(&page_size)
            || !page_size.is_power_of_two()
            || !base.len().is_multiple_of(page_size as u64)
            || base.len() > i64::MAX as u64
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            len: base.len(),
            base_limit: base.len(),
            base,
            page_size,
            pages: BTreeMap::new(),
            max_pages: max_dirty_bytes / page_size,
            frozen: false,
        })
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn page(&self, n: u64) -> Result<Vec<u8>> {
        if let Some(p) = self.pages.get(&n) {
            return Ok(p.clone());
        }
        let mut p = vec![0; self.page_size];
        let start = n.checked_mul(self.page_size as u64).ok_or(Error::Invalid)?;
        let size = self
            .base_limit
            .saturating_sub(start)
            .min(self.page_size as u64) as usize;
        if size != 0 {
            self.base.read_exact_at(start, &mut p[..size])?;
        }
        Ok(p)
    }
    /// Returns false only for a genuine EOF short read. The remaining bytes are zero.
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<bool> {
        out.fill(0);
        let size = self.len.saturating_sub(offset).min(out.len() as u64) as usize;
        let mut done = 0;
        while done < size {
            let at = offset + done as u64;
            let within = (at % self.page_size as u64) as usize;
            let count = (size - done).min(self.page_size - within);
            let mut io = observability::PageIo::new("pager", "read", at / self.page_size as u64);
            let page = match self.page(at / self.page_size as u64) {
                Ok(page) => page,
                Err(error) => {
                    io.finish("failed", 0);
                    return Err(error);
                }
            };
            io.finish("success", count as u64);
            out[done..done + count].copy_from_slice(&page[within..within + count]);
            done += count;
        }
        Ok(size == out.len())
    }
    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        if self.frozen {
            return Err(Error::Frozen);
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(Error::Invalid)?;
        // Include extension holes explicitly, so later publication cannot expose base bytes.
        let first = offset.min(self.len) / self.page_size as u64;
        let last = (end - 1) / self.page_size as u64;
        let missing = (first..=last)
            .filter(|n| !self.pages.contains_key(n))
            .take(self.max_pages.saturating_add(1))
            .count();
        if missing > self.max_pages.saturating_sub(self.pages.len()) {
            return Err(Error::Budget);
        }
        // Stage reads before mutation: failed remote reads cannot leave a partial write.
        let mut staged = BTreeMap::new();
        for n in first..=last {
            let mut p = self.page(n)?;
            let start = n * self.page_size as u64;
            let lo = offset.max(start);
            let hi = end.min(start + self.page_size as u64);
            if hi > lo {
                p[(lo - start) as usize..(hi - start) as usize]
                    .copy_from_slice(&bytes[(lo - offset) as usize..(hi - offset) as usize]);
            }
            staged.insert(n, p);
        }
        for (page, bytes) in staged {
            let mut io = observability::PageIo::new("pager", "write", page);
            let len = bytes.len() as u64;
            self.pages.insert(page, bytes);
            io.finish("success", len);
        }
        self.len = self.len.max(end);
        Ok(())
    }
    pub fn truncate(&mut self, len: u64) -> Result<()> {
        if self.frozen {
            return Err(Error::Frozen);
        }
        if len > i64::MAX as u64 {
            return Err(Error::Invalid);
        }
        if len > self.len {
            return self.write(len - 1, &[0]);
        }
        if len == self.len {
            return Ok(());
        }
        let n = len / self.page_size as u64;
        let partial = (len % self.page_size as u64) as usize;
        let tail = if partial != 0 {
            if !self.pages.contains_key(&n) && self.pages.len() >= self.max_pages {
                return Err(Error::Budget);
            }
            let mut p = self.page(n)?;
            p[partial..].fill(0);
            Some(p)
        } else {
            None
        };
        self.pages.retain(|page, _| *page < n);
        if let Some(p) = tail {
            self.pages.insert(n, p);
        }
        self.len = len;
        self.base_limit = self.base_limit.min(len);
        Ok(())
    }
    pub fn freeze(&mut self) -> Result<FrozenPages> {
        if self.frozen {
            return Err(Error::Frozen);
        }
        if !self.len.is_multiple_of(self.page_size as u64) {
            return Err(Error::Invalid);
        }
        self.frozen = true;
        Ok(FrozenPages {
            page_size: self.page_size,
            final_file_size: self.len,
            pages: std::mem::take(&mut self.pages),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Bytes(Vec<u8>);
    impl Snapshot for Bytes {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<()> {
            out.copy_from_slice(&self.0[offset as usize..offset as usize + out.len()]);
            Ok(())
        }
    }
    #[test]
    fn partial_cross_page_eof_and_truncate_regrow() {
        for size in [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            let mut f = CowFile::new(Arc::new(Bytes(vec![7; size * 3])), size, size * 5).unwrap();
            f.write(size as u64 - 2, &[1, 2, 3, 4]).unwrap();
            let mut out = [0; 6];
            assert!(f.read(size as u64 - 3, &mut out).unwrap());
            assert_eq!(out, [7, 1, 2, 3, 4, 7]);
            f.truncate(size as u64 + 1).unwrap();
            f.truncate((size * 3) as u64).unwrap();
            let mut out = vec![9; size * 2];
            assert!(f.read(size as u64 + 1, &mut out).is_ok());
            assert!(out.iter().all(|v| *v == 0));
            assert!(!f.read((size * 3) as u64 - 1, &mut [0; 2]).unwrap());
            let frozen = f.freeze().unwrap();
            assert_eq!(frozen.pages.len(), 3);
            assert!(f.write(0, &[1]).is_err());
        }
    }
    #[test]
    fn budget_is_atomic_and_missing_base_is_not_zero() {
        let mut f = CowFile::new(Arc::new(Bytes(vec![])), 512, 512).unwrap();
        assert!(matches!(f.write(1024, &[1]), Err(Error::Budget)));
        assert_eq!(f.len(), 0);
        struct Missing;
        impl Snapshot for Missing {
            fn len(&self) -> u64 {
                512
            }
            fn read_exact_at(&self, _: u64, _: &mut [u8]) -> Result<()> {
                Err(Error::Io("missing".into()))
            }
        }
        let f = CowFile::new(Arc::new(Missing), 512, 512).unwrap();
        assert!(matches!(f.read(0, &mut [0; 1]), Err(Error::Io(_))));
    }

    struct FailingPage {
        bytes: Bytes,
        fail: std::sync::atomic::AtomicBool,
    }
    impl Snapshot for FailingPage {
        fn len(&self) -> u64 {
            self.bytes.len()
        }
        fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<()> {
            if offset == 512 && self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(Error::Io("injected second-page failure".into()));
            }
            self.bytes.read_exact_at(offset, out)
        }
    }

    #[test]
    fn failed_cross_page_write_preserves_prior_changes_and_can_retry() {
        let base = Arc::new(FailingPage {
            bytes: Bytes(vec![7; 1024]),
            fail: std::sync::atomic::AtomicBool::new(true),
        });
        let mut file = CowFile::new(base.clone(), 512, 1536).unwrap();
        file.write(0, &[3]).unwrap();
        // The first page is already dirty; neither it nor the length may change
        // when fetching the next page fails, even for an extending write.
        assert!(matches!(file.write(510, &[9; 516]), Err(Error::Io(_))));
        assert_eq!(file.len(), 1024);
        base.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        let mut bytes = vec![0; 1024];
        assert!(file.read(0, &mut bytes).unwrap());
        let mut expected = vec![7; 1024];
        expected[0] = 3;
        assert_eq!(bytes, expected);

        file.write(510, &[9; 516]).unwrap();
        file.truncate(1536).unwrap();
        let frozen = file.freeze().unwrap();
        expected.resize(1536, 0);
        expected[510..1026].fill(9);
        assert_eq!(frozen.final_file_size, 1536);
        assert_eq!(
            frozen.pages.into_values().flatten().collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn failed_partial_truncate_preserves_length_and_base_watermark() {
        for fail_io in [false, true] {
            let base = Arc::new(FailingPage {
                bytes: Bytes(vec![7; 1536]),
                fail: std::sync::atomic::AtomicBool::new(fail_io),
            });
            let mut file =
                CowFile::new(base.clone(), 512, if fail_io { 1536 } else { 512 }).unwrap();
            file.write(0, &[3]).unwrap();
            let result = file.truncate(513);
            if fail_io {
                assert!(matches!(result, Err(Error::Io(_))));
            } else {
                assert!(matches!(result, Err(Error::Budget)));
            }
            base.fail.store(false, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(file.len(), 1536);
            let mut bytes = vec![0; 1536];
            assert!(file.read(0, &mut bytes).unwrap());
            let mut expected = vec![7; 1536];
            expected[0] = 3;
            assert_eq!(bytes, expected);
            let frozen = file.freeze().unwrap();
            assert_eq!(frozen.pages.keys().copied().collect::<Vec<_>>(), vec![0]);
            assert_eq!(frozen.pages[&0], expected[..512]);
        }
    }
}

pub mod bridge;
