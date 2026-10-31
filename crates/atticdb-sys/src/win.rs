//! Win32 implementations. Each public function is safe to call with any argument.

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, ERROR_LOCK_VIOLATION};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, LockFileEx, MoveFileExW, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK,
    LOCKFILE_FAIL_IMMEDIATELY, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows_sys::Win32::System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};

/// The 1 byte region the data directory lock covers. It sits far past any real end of file.
const LOCK_OFFSET: u64 = 0x7FFF_FFFF_FFFF_FFFE;

/// Converts a path to a NUL-terminated UTF-16 string. Interior NULs are rejected.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL character"));
    }
    wide.push(0);
    Ok(wide)
}

/// Returns the bytes available to the caller and the volume's total bytes, in that order.
///
/// Uses `GetDiskFreeSpaceExW`. The available count honors per-user quotas.
pub fn free_space(path: &Path) -> io::Result<(u64, u64)> {
    let path = wide(path)?;
    let (mut available, mut total) = (0u64, 0u64);
    // SAFETY: `path` is a NUL-terminated UTF-16 string that outlives the call.
    // Both out pointers refer to live locals. The API allows a null third pointer.
    let ok = unsafe {
        GetDiskFreeSpaceExW(path.as_ptr(), &mut available, &mut total, std::ptr::null_mut())
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((available, total))
}

/// Renames `from` to `to`, replacing `to` if it exists.
///
/// Uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`.
/// Write-through makes the call return only after the rename is flushed to disk.
pub fn rename_write_through(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (wide(from)?, wide(to)?);
    // SAFETY: both arguments are NUL-terminated UTF-16 strings that outlive the call.
    let ok = unsafe {
        MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Fills `buf` from the system preferred random number generator.
///
/// Uses `BCryptGenRandom` with `BCRYPT_USE_SYSTEM_PREFERRED_RNG` and no algorithm handle.
/// Buffers longer than `u32::MAX` bytes are filled in chunks.
pub fn fill_random(buf: &mut [u8]) -> io::Result<()> {
    for chunk in buf.chunks_mut(u32::MAX as usize) {
        // `chunks_mut` never yields more than `u32::MAX` bytes, so this cast is exact.
        let len = chunk.len() as u32;
        // SAFETY: `chunk` is a live, writable buffer of exactly `len` bytes.
        // A null algorithm handle is valid with BCRYPT_USE_SYSTEM_PREFERRED_RNG.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                len,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        // NTSTATUS values below 0 are errors.
        if status < 0 {
            return Err(io::Error::other(format!(
                "BCryptGenRandom failed with NTSTATUS {status:#010x}"
            )));
        }
    }
    Ok(())
}

/// Builds the request for the lock region. Every other field stays zero.
fn lock_region() -> OVERLAPPED {
    OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: LOCK_OFFSET as u32,
                OffsetHigh: (LOCK_OFFSET >> 32) as u32,
            },
        },
        ..Default::default()
    }
}

/// Tries to take an exclusive lock on a 1 byte region far past the end of `file`.
///
/// Returns `Ok(true)` when this handle now holds the lock.
/// Returns `Ok(false)` when another handle holds it.
///
/// Uses `LockFileEx` with `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`.
/// An exclusive lock denies other processes both read and write access to its region.
/// Locking a region past the end of file is not an error.
/// So the lock never covers real bytes, and other processes can still read the file.
/// AtticDB keeps the holder's details at offset 0 of its LOCK file.
/// `File::try_lock` would lock the whole file and block those reads.
///
/// Locks belong to the handle. Release one with [`unlock_past_eof`] before closing the file.
/// Windows may release a closed handle's locks only after a delay.
pub fn try_lock_past_eof(file: &File) -> io::Result<bool> {
    let mut region = lock_region();
    // SAFETY: `file` owns the handle and keeps it open for the call. `region` is a live
    // local. The flags make the call fail at once instead of waiting, so no request stays
    // pending after it returns.
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut region,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // A synchronous handle reports contention as ERROR_LOCK_VIOLATION.
        // A handle opened for overlapped I/O reports it as ERROR_IO_PENDING.
        Some(code) if code == ERROR_LOCK_VIOLATION as i32 || code == ERROR_IO_PENDING as i32 => {
            Ok(false)
        }
        _ => Err(error),
    }
}

/// Releases the lock that [`try_lock_past_eof`] took on this handle.
pub fn unlock_past_eof(file: &File) -> io::Result<()> {
    let mut region = lock_region();
    // SAFETY: `file` owns the handle and keeps it open for the call. `region` is a live local.
    let ok = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut region) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "atticdb-sys-{}-{}",
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

    #[test]
    fn free_space_reports_available_within_total() {
        let (available, total) = free_space(&std::env::temp_dir()).unwrap();
        assert!(total > 0);
        assert!(available <= total, "available={available} total={total}");
    }

    #[test]
    fn paths_with_interior_nul_are_rejected() {
        let error = free_space(Path::new("a\0b")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let error = rename_write_through(Path::new("a"), Path::new("b\0")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rename_write_through_replaces_an_existing_file() {
        let dir = TestDir::new();
        let (from, to) = (dir.0.join("from"), dir.0.join("to"));
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        rename_write_through(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.try_exists().unwrap());
        assert!(rename_write_through(&from, &to).is_err());
    }

    #[test]
    fn fill_random_fills_and_varies() {
        let (mut first, mut second) = ([0u8; 64], [0u8; 64]);
        fill_random(&mut first).unwrap();
        fill_random(&mut second).unwrap();
        assert!(first.iter().any(|&byte| byte != 0));
        assert_ne!(first, second);
        fill_random(&mut []).unwrap();
    }

    #[test]
    fn lock_past_eof_excludes_other_handles_but_not_readers() {
        let dir = TestDir::new();
        let path = dir.0.join("LOCK");
        std::fs::write(&path, b"pid=1 holder").unwrap();
        let open = || std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let (a, mut b) = (open(), open());
        assert!(try_lock_past_eof(&a).unwrap());
        assert!(!try_lock_past_eof(&b).unwrap());
        let mut holder = String::new();
        b.read_to_string(&mut holder).unwrap();
        assert_eq!(holder, "pid=1 holder");
        unlock_past_eof(&a).unwrap();
        assert!(try_lock_past_eof(&b).unwrap());
        assert!(!try_lock_past_eof(&a).unwrap());
        unlock_past_eof(&b).unwrap();
        assert!(unlock_past_eof(&b).is_err(), "unlocking an unheld region fails");
    }
}
