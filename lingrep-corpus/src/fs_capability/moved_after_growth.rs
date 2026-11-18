//! A SQLite VFS that delegates to the default VFS but reports a main database file as moved
//! (`SQLITE_FCNTL_HAS_MOVED`) once its size differs from its size when the connection opened it.
//!
//! On macOS 15.8's FSKit msdos driver, a new database file keeps a placeholder inode number
//! until a WAL checkpoint writes it, so SQLite's unix VFS gives that answer for the open
//! connection; this VFS gives the same answer on any filesystem.

use std::{
    collections::HashMap,
    ffi::{CStr, c_int, c_void},
    sync::{Mutex, OnceLock},
};

use libsqlite3_sys as ffi;

const NAME_C: &CStr = c"lingrep-moved-after-growth";

/// The name to pass to `SqliteConnectOptions::vfs`.
pub(crate) fn name() -> &'static str {
    NAME_C.to_str().unwrap()
}

struct OpenFile {
    size_at_open: i64,
    methods: *const ffi::sqlite3_io_methods,
}

// SAFETY: the raw pointers address SQLite's static method tables and leaked copies of them,
// which are never freed or mutated.
unsafe impl Send for OpenFile {}
struct Methods(*const ffi::sqlite3_io_methods);
// SAFETY: as above.
unsafe impl Send for Methods {}

struct State {
    open: HashMap<usize, OpenFile>,
    wrapped: HashMap<usize, Methods>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            open: HashMap::new(),
            wrapped: HashMap::new(),
        })
    })
}

/// Registers the VFS under [`name`] once per process.
pub(crate) fn register() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        // SAFETY: sqlite3_vfs_find(NULL) returns the default VFS, which lives for the
        // process; the copy is leaked so SQLite may keep its pointer.
        unsafe {
            assert_eq!(ffi::sqlite3_initialize(), ffi::SQLITE_OK);
            let default = ffi::sqlite3_vfs_find(std::ptr::null());
            assert!(!default.is_null());
            let mut vfs = *default;
            vfs.zName = NAME_C.as_ptr();
            vfs.pAppData = default.cast::<c_void>();
            vfs.xOpen = Some(open);
            let vfs = Box::into_raw(Box::new(vfs));
            assert_eq!(ffi::sqlite3_vfs_register(vfs, 0), ffi::SQLITE_OK);
        }
    });
}

unsafe fn file_size(file: *mut ffi::sqlite3_file, methods: *const ffi::sqlite3_io_methods) -> i64 {
    let mut size = 0;
    // SAFETY: the caller passes an open file and the method table it was opened with.
    let rc = unsafe { ((*methods).xFileSize.expect("xFileSize"))(file, &raw mut size) };
    assert_eq!(rc, ffi::SQLITE_OK);
    size
}

unsafe extern "C" fn open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    // SAFETY: pAppData is the default VFS this one copied, and szOsFile is unchanged, so
    // `file` has room for the default VFS's file object.
    unsafe {
        let default = (*vfs).pAppData.cast::<ffi::sqlite3_vfs>();
        let rc = ((*default).xOpen.expect("xOpen"))(default, name, file, flags, out_flags);
        if rc != ffi::SQLITE_OK || flags & ffi::SQLITE_OPEN_MAIN_DB == 0 {
            return rc;
        }
        let methods = (*file).pMethods;
        let size_at_open = file_size(file, methods);
        let mut state = state().lock().unwrap();
        let wrapped = state
            .wrapped
            .entry(methods as usize)
            .or_insert_with(|| {
                let mut copy = *methods;
                copy.xClose = Some(close);
                copy.xFileControl = Some(file_control);
                Methods(Box::into_raw(Box::new(copy)))
            })
            .0;
        state.open.insert(
            file as usize,
            OpenFile {
                size_at_open,
                methods,
            },
        );
        (*file).pMethods = wrapped;
        rc
    }
}

unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    let opened = state().lock().unwrap().open.remove(&(file as usize));
    let opened = opened.expect("close of a file this VFS did not open");
    // SAFETY: the file was opened with these methods, which own its close.
    unsafe {
        (*file).pMethods = opened.methods;
        ((*opened.methods).xClose.expect("xClose"))(file)
    }
}

unsafe extern "C" fn file_control(
    file: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    let (size_at_open, methods) = {
        let state = state().lock().unwrap();
        let opened = &state.open[&(file as usize)];
        (opened.size_at_open, opened.methods)
    };
    // SAFETY: `file` is open with `methods`; HAS_MOVED's argument is a pointer to int.
    unsafe {
        if op == ffi::SQLITE_FCNTL_HAS_MOVED {
            *arg.cast::<c_int>() = c_int::from(file_size(file, methods) != size_at_open);
            return ffi::SQLITE_OK;
        }
        ((*methods).xFileControl.expect("xFileControl"))(file, op, arg)
    }
}
