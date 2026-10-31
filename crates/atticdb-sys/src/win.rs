//! Win32 implementations. Each public function is safe to call with any argument.

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, Prefix};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetHandleInformation, SetHandleInformation, ERROR_IO_PENDING,
    ERROR_LOCK_VIOLATION, ERROR_OPERATION_ABORTED, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, LockFileEx, MoveFileExW, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK,
    LOCKFILE_FAIL_IMMEDIATELY, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows_sys::Win32::System::SystemServices::PROCESS_MITIGATION_STRICT_HANDLE_CHECK_POLICY;
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetProcessMitigationPolicy, ProcessStrictHandleCheckPolicy,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0,
};

/// The 1 byte region the data directory lock covers. It sits far past any real end of file.
const LOCK_OFFSET: u64 = 0x7FFF_FFFF_FFFF_FFFE;

/// The longest path, in UTF-16 units without the NUL, that Win32 accepts without `\\?\`.
const MAX_PATH_UNITS: usize = 259;

fn encode(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}

/// Converts a path to a NUL-terminated UTF-16 string. Interior NULs are rejected.
///
/// Relative paths and paths longer than `MAX_PATH_UNITS` go through [`resolve`].
/// Win32 limits a relative path to MAX_PATH after joining it to the current directory.
/// `\\?\` can't lift that, because it doesn't accept relative paths.
/// std also resolves relative paths with `GetFullPathNameW` before prefixing.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide = encode(path);
    if wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL character"));
    }
    if wide.len() > MAX_PATH_UNITS || !path.is_absolute() {
        wide = resolve(path)?;
    }
    wide.push(0);
    Ok(wide)
}

/// Makes `path` absolute, then adds `\\?\` if it is still longer than `MAX_PATH_UNITS`.
///
/// `std::path::absolute` calls `GetFullPathNameW`, which resolves the current directory,
/// `.`, `..` and `/`. That matters because `\\?\` turns off Win32 path parsing.
/// Paths that already start with `\\?\` or `\\.\` get no second prefix.
fn resolve(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let units = encode(&absolute);
    if units.len() <= MAX_PATH_UNITS {
        return Ok(units);
    }
    let Some(Component::Prefix(prefix)) = absolute.components().next() else {
        return Ok(units);
    };
    let (head, rest) = match prefix.kind() {
        // C:\dir becomes \\?\C:\dir.
        Prefix::Disk(_) => (r"\\?\", &units[..]),
        // \\server\share becomes \\?\UNC\server\share. Keep the second backslash.
        Prefix::UNC(..) => (r"\\?\UNC", &units[1..]),
        _ => return Ok(units),
    };
    Ok(head.encode_utf16().chain(rest.iter().copied()).collect())
}

/// Returns the bytes available to the caller and the volume's total bytes, in that order.
///
/// Uses `GetDiskFreeSpaceExW`. The available count honors per-user quotas.
/// With a per-user quota, the total is the quota, not the volume size.
/// So AtticDB's free-space floor follows the quota, unlike statvfs on Unix.
/// `path` must name a directory. A trailing separator is added, which UNC shares require.
pub fn free_space(path: &Path) -> io::Result<(u64, u64)> {
    let path = wide(&path.join(""))?;
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

/// Stops later child processes from inheriting this process's stdin, stdout and stderr.
///
/// This process inherited them, so they are inheritable here too. std starts every child
/// with handle inheritance on. So each child gets a copy, even one whose stdio is a file.
/// A server that outlives this process would then hold its caller's pipes open.
/// The caller would never see end of file.
/// std duplicates a handle for each child that should get it, so `Stdio::inherit` still works.
/// Best effort: a null or invalid handle is skipped, and a failed call changes nothing.
pub fn disinherit_stdio() {
    let handles =
        [io::stdin().as_raw_handle(), io::stdout().as_raw_handle(), io::stderr().as_raw_handle()];
    for handle in handles {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: SetHandleInformation only reads the handle value and changes its flags.
        // A handle that is not open makes the call fail, which changes nothing.
        let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
}

/// The highest handle value [`disinherit_all`] checks. Handle values are multiples of 4, so
/// this covers a process with up to 65,536 handles.
const MAX_HANDLE_VALUE: usize = 1 << 18;

/// `RaiseExceptionOnInvalidHandleReference`, bit 0 of the strict handle check policy's flags.
const RAISE_ON_INVALID_HANDLE: u32 = 1;

/// Whether this process raises an exception when it uses a handle value that is not valid.
/// Exploit Protection's "Validate handle usage" turns that on. Then probing handle values would
/// crash the process. A failed query counts as on. See
/// <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocessmitigationpolicy>.
fn strict_handle_checks() -> bool {
    let mut policy = PROCESS_MITIGATION_STRICT_HANDLE_CHECK_POLICY::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle, which needs no closing. The call
    // writes at most `size_of_val(&policy)` bytes, into `policy` on this stack frame.
    let read = unsafe {
        GetProcessMitigationPolicy(
            GetCurrentProcess(),
            ProcessStrictHandleCheckPolicy,
            (&raw mut policy).cast(),
            size_of_val(&policy),
        )
    };
    // SAFETY: both union members are 32 bits wide, so `Flags` reads exactly those bits.
    read == 0 || unsafe { policy.Anonymous.Flags } & RAISE_ON_INVALID_HANDLE != 0
}

/// Stops later child processes from inheriting any handle of this process, not only its
/// stdio.
///
/// A parent may pass down more inheritable handles than stdio. PowerShell does, when it runs a
/// program with its output redirected: the program also gets PowerShell's own stdout. A server
/// that outlives this process would then hold that pipe open, and PowerShell's caller would
/// wait for end of file. std still gives each child the stdio its command sets, as it
/// duplicates those handles for that child. Best effort: values above [`MAX_HANDLE_VALUE`] are
/// not checked, and a failed call changes nothing. A handle value above the cap stays
/// inheritable; the cost is that the first chat after a cold start may get no memory block.
///
/// Under strict handle checks, by [`strict_handle_checks`], a probe of a value that is not a
/// handle would raise an exception. Then it clears only the stdio, by [`disinherit_stdio`].
///
/// It doesn't take std's spawn lock. A `Command::spawn` on another thread at the same moment
/// could lose its stdio, as std's duplicated handles would stop being inheritable. So call it
/// only from the 1 thread that spawns, right before it spawns. AtticDB calls it only to start a
/// server, behind 1 `Launch`, which no other thread spawns beside.
pub fn disinherit_all() {
    disinherit_stdio();
    if strict_handle_checks() {
        return;
    }
    for value in (4..=MAX_HANDLE_VALUE).step_by(4) {
        let handle: HANDLE = std::ptr::without_provenance_mut(value);
        let mut flags = 0;
        // SAFETY: GetHandleInformation only reads the flags of `handle` into `flags`. A value
        // that is not an open handle makes the call fail, which changes nothing. Strict handle
        // checks, which would raise an exception instead, are off.
        let inheritable = unsafe { GetHandleInformation(handle, &mut flags) } != 0
            && flags & HANDLE_FLAG_INHERIT != 0;
        if inheritable {
            // SAFETY: another thread may close `handle` after the check, or reuse its value for
            // a new handle. Either is harmless. The call only clears the inherit flag, and a
            // closed value makes it fail, which changes nothing.
            let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

/// A manual-reset event that one lock request signals when it completes.
struct Event(HANDLE);

impl Event {
    fn new() -> io::Result<Self> {
        // SAFETY: null security attributes and a null name are both allowed.
        // The call creates a new, unnamed, nonsignaled manual-reset event.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(event))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from CreateEventW and is closed only here, once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Builds the request for the lock region. `event` must outlive any wait on the request.
///
/// A request with its own event can be waited on safely. Other I/O can signal the file handle.
/// The event's low bit is set, so a completion port bound to the handle queues no packet.
/// Windows ignores the low 2 bits of a handle value.
fn lock_region(event: &Event) -> OVERLAPPED {
    OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: LOCK_OFFSET as u32,
                OffsetHigh: (LOCK_OFFSET >> 32) as u32,
            },
        },
        hEvent: event.0.map_addr(|addr| addr | 1),
        ..Default::default()
    }
}

/// Waits until a request that returned ERROR_IO_PENDING completes, and returns its result.
///
/// Only a handle opened with `FILE_FLAG_OVERLAPPED` returns ERROR_IO_PENDING.
/// The kernel then owns `region` until the request completes.
/// So `region` must not be dropped before this returns.
/// With `cancel`, it first asks the kernel to cancel the request.
fn finish_pending(file: &File, region: &mut OVERLAPPED, cancel: bool) -> io::Result<()> {
    if cancel {
        // SAFETY: `file` keeps the handle open, and `region` is the pending request's live
        // OVERLAPPED. CancelIoEx only uses it to find the request. It fails if the request
        // already completed, and the wait below covers both cases.
        unsafe { CancelIoEx(file.as_raw_handle(), region) };
    }
    let mut transferred = 0u32;
    // SAFETY: `file` keeps the handle open. `region` and `transferred` are live. `region`
    // names its own event, so a wait of TRUE returns only once this request completes.
    // After that the kernel holds no reference to `region`.
    let ok = unsafe { GetOverlappedResult(file.as_raw_handle(), region, &mut transferred, 1) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn is(error: &io::Error, code: u32) -> bool {
    error.raw_os_error() == Some(code as i32)
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
/// Any `File` works, including one opened with `FILE_FLAG_OVERLAPPED`.
/// On such a handle a pending request is cancelled and awaited before this returns.
/// A cancelled request reports `Ok(false)`. One that completed first reports its own result.
///
/// Locks belong to the handle. Release one with [`unlock_past_eof`] before closing the file.
/// Windows may release a closed handle's locks only after a delay.
pub fn try_lock_past_eof(file: &File) -> io::Result<bool> {
    let event = Event::new()?;
    let mut region = lock_region(&event);
    // SAFETY: `file` keeps the handle open, and `region` and `event` are live locals.
    // A synchronous handle finishes the request before returning. An overlapped handle can
    // leave it pending, and then `finish_pending` waits for it before `region` is dropped.
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
    let mut error = io::Error::last_os_error();
    if is(&error, ERROR_IO_PENDING) {
        match finish_pending(file, &mut region, true) {
            Ok(()) => return Ok(true),
            Err(finished) => error = finished,
        }
    }
    if is(&error, ERROR_LOCK_VIOLATION) || is(&error, ERROR_OPERATION_ABORTED) {
        return Ok(false);
    }
    Err(error)
}

/// Releases the lock that [`try_lock_past_eof`] took on this handle.
pub fn unlock_past_eof(file: &File) -> io::Result<()> {
    let event = Event::new()?;
    let mut region = lock_region(&event);
    // SAFETY: `file` keeps the handle open, and `region` and `event` are live locals.
    // A pending request is awaited below before `region` is dropped.
    let ok = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut region) };
    if ok != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if is(&error, ERROR_IO_PENDING) {
        return finish_pending(file, &mut region, false);
    }
    Err(error)
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

    #[test]
    fn lock_past_eof_works_on_overlapped_handles() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;
        let dir = TestDir::new();
        let path = dir.0.join("LOCK");
        std::fs::write(&path, b"pid=1 holder").unwrap();
        let mut options = std::fs::OpenOptions::new();
        let sync = options.read(true).write(true).open(&path).unwrap();
        let overlapped = options.custom_flags(FILE_FLAG_OVERLAPPED).open(&path).unwrap();
        assert!(try_lock_past_eof(&sync).unwrap());
        assert!(!try_lock_past_eof(&overlapped).unwrap());
        unlock_past_eof(&sync).unwrap();
        assert!(try_lock_past_eof(&overlapped).unwrap());
        assert!(!try_lock_past_eof(&sync).unwrap());
        unlock_past_eof(&overlapped).unwrap();
        assert!(try_lock_past_eof(&sync).unwrap());
        unlock_past_eof(&sync).unwrap();
    }

    fn os(units: Vec<u16>) -> String {
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn paths_resolve_and_get_a_prefix_only_when_long() {
        let a = "a".repeat(MAX_PATH_UNITS);
        let cases = [
            // Short absolute paths pass through. Win32 parses them itself.
            (r"C:\short".to_owned(), r"C:\short".to_owned()),
            (r"C:\a\..\b".to_owned(), r"C:\a\..\b".to_owned()),
            (format!(r"C:\{a}"), format!(r"\\?\C:\{a}")),
            (format!("C:/{a}/b"), format!(r"\\?\C:\{a}\b")),
            (format!(r"\\server\share\{a}"), format!(r"\\?\UNC\server\share\{a}")),
            (format!(r"\\?\C:\{a}\..\b"), format!(r"\\?\C:\{a}\..\b")),
            // Long input that resolves short needs no prefix.
            (format!(r"C:\{a}\..\b"), r"C:\b".to_owned()),
        ];
        for (path, expected) in cases {
            assert_eq!(os(wide(Path::new(&path)).unwrap()), format!("{expected}\0"), "{path}");
        }
        // A short relative path resolves against the current directory.
        let resolved = std::env::current_dir().unwrap().join("relative");
        let expected = format!("{}\0", resolved.display());
        assert_eq!(os(wide(Path::new("relative")).unwrap()), expected);
    }

    /// Calls MoveFileExW with the path as given, as the code did before relative paths resolved.
    fn raw_rename(from: &str, to: &str) -> io::Result<()> {
        let nul = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (from, to) = (nul(from), nul(to));
        // SAFETY: both arguments are NUL-terminated UTF-16 strings that outlive the call.
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Runs in a child process, because the current directory is global to the process.
    fn short_relative_paths_under_a_deep_cwd(dir: &Path) {
        // Windows caps the current directory below MAX_PATH, so stop at 220 units.
        let mut deep = dir.to_path_buf();
        while deep.as_os_str().len() < 220 {
            let room = 220 - deep.as_os_str().len();
            deep.push("c".repeat(room.clamp(1, 50)));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::env::set_current_dir(&deep).unwrap();
        let (from, to, sub) = ("f".repeat(60), "t".repeat(60), "s".repeat(60));
        let full = std::path::absolute(&to).unwrap();
        assert!(full.as_os_str().len() > MAX_PATH_UNITS, "{}", full.display());
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        // Control: Win32 rejects these paths unresolved, so this test reaches the limit.
        assert!(raw_rename(&from, &to).is_err(), "raw relative rename should fail");
        rename_write_through(Path::new(&from), Path::new(&to)).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!Path::new(&from).try_exists().unwrap());
        std::fs::create_dir(&sub).unwrap();
        let (available, total) = free_space(Path::new(&sub)).unwrap();
        assert!(total > 0 && available <= total, "available={available} total={total}");
        std::fs::write(dir.join("child-ran"), b"").unwrap();
    }

    #[test]
    fn short_relative_paths_resolve_under_a_deep_cwd() {
        const CHILD: &str = "ATTICDB_SYS_DEEP_CWD_CHILD";
        if let Some(dir) = std::env::var_os(CHILD) {
            return short_relative_paths_under_a_deep_cwd(Path::new(&dir));
        }
        let dir = TestDir::new();
        let name = "win::tests::short_relative_paths_resolve_under_a_deep_cwd";
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(CHILD, &dir.0)
            .status()
            .unwrap();
        assert!(status.success(), "child failed: {status}");
        // An empty filter match would also exit 0. The marker proves the child ran.
        assert!(dir.0.join("child-ran").try_exists().unwrap(), "child test did not run");
    }

    #[test]
    fn long_paths_work_for_rename_and_free_space() {
        let dir = TestDir::new();
        let mut deep = dir.0.clone();
        for _ in 0..5 {
            deep.push("d".repeat(60));
        }
        std::fs::create_dir_all(&deep).unwrap();
        let (from, to) = (deep.join("from"), deep.join("to"));
        assert!(to.as_os_str().len() > 260, "{}", to.display());
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        rename_write_through(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.try_exists().unwrap());
        // Separators and `..` are resolved before the `\\?\` prefix turns parsing off.
        let back = PathBuf::from(format!("{}/x/../back", deep.display()));
        rename_write_through(&to, &back).unwrap();
        assert_eq!(std::fs::read(deep.join("back")).unwrap(), b"new");
        let (available, total) = free_space(&deep).unwrap();
        assert!(total > 0 && available <= total, "available={available} total={total}");
    }

    /// An inheritable handle stops being inheritable. A child still gets inherited stdio
    /// afterwards, as std duplicates the handles for it.
    #[test]
    fn disinherited_stdio_still_reaches_children() {
        let dir = std::env::temp_dir().join(format!("atticdb-sys-inherit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = File::create(dir.join("f")).unwrap();
        let handle = file.as_raw_handle();
        let flags = || {
            let mut flags = 0;
            // SAFETY: `handle` stays open while `file` lives, and the call only reads its flags.
            assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
            flags & HANDLE_FLAG_INHERIT
        };
        // SAFETY: `handle` is open, and the call only changes its flags.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        assert_eq!(flags(), HANDLE_FLAG_INHERIT);
        disinherit_all();
        // Under strict handle checks it clears only the stdio.
        let left = if strict_handle_checks() { HANDLE_FLAG_INHERIT } else { 0 };
        assert_eq!(flags(), left);
        drop(file);
        std::fs::remove_dir_all(&dir).unwrap();
        disinherit_stdio();
        disinherit_stdio();
        let root = std::env::var_os("SystemRoot").unwrap();
        let cmd = PathBuf::from(root).join("System32").join("cmd.exe");
        let status = std::process::Command::new(cmd).args(["/C", "exit 7"]).status().unwrap();
        assert_eq!(status.code(), Some(7));
    }
}
