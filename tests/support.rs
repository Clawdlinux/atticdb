use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub(crate) struct TestDir(pub(crate) PathBuf);

impl TestDir {
    #[track_caller]
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(ROOT).join(format!(
            "atticdb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
