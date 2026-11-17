//! The process's open-file limit (`RLIMIT_NOFILE`).

/// Attempts to raise the soft open-file limit to the hard limit, capped at
/// `kern.maxfilesperproc` on macOS; unsupported platforms and system-call errors are ignored.
pub fn raise_soft_limit() {
    #[cfg(unix)]
    {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid, writable `rlimit`.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
            return;
        }
        #[cfg(target_os = "macos")]
        let target = max_files_per_process().map_or(limit.rlim_max, |cap| limit.rlim_max.min(cap));
        #[cfg(not(target_os = "macos"))]
        let target = limit.rlim_max;
        if target > limit.rlim_cur {
            limit.rlim_cur = target;
            // SAFETY: `limit` is a valid `rlimit`; failure leaves the limit unchanged.
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) };
        }
    }
}

#[cfg(target_os = "macos")]
fn max_files_per_process() -> Option<libc::rlim_t> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated, and `value` and `size` describe a
    // writable `c_int`, the type of `kern.maxfilesperproc`.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&raw mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && value > 0).then(|| libc::rlim_t::from(value.unsigned_abs()))
}

/// The soft open-file limit and the descriptors this process has open.
#[derive(Clone, Copy, Debug)]
pub struct OpenFiles {
    pub limit: usize,
    pub open: usize,
}

impl OpenFiles {
    /// `None` where the limit or the open descriptors cannot be read.
    pub fn current() -> Option<Self> {
        #[cfg(unix)]
        {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: `limit` is a valid, writable `rlimit`.
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
                return None;
            }
            let limit = usize::try_from(limit.rlim_cur).ok()?;
            let open = std::fs::read_dir("/dev/fd").ok()?.count();
            Some(Self { limit, open })
        }
        #[cfg(not(unix))]
        None
    }

    /// The files that can still be opened under the soft limit.
    pub fn available(self) -> usize {
        self.limit.saturating_sub(self.open)
    }
}
