use std::fs::{self, File, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// Restores a file's original permissions when dropped, including during unwinding.
pub struct UnreadableFile {
    path: PathBuf,
    permissions: Permissions,
}

impl UnreadableFile {
    /// Sets mode 000, returning `None` with permissions restored if this process can still read it.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors other than the expected permission denial from the read probe.
    pub fn new(path: &Path) -> io::Result<Option<Self>> {
        let permissions = fs::metadata(path)?.permissions();
        fs::set_permissions(path, Permissions::from_mode(0o000))?;
        let guard = Self {
            path: path.to_owned(),
            permissions,
        };
        match File::open(path) {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(Some(guard)),
            Err(error) => Err(error),
            Ok(_) => Ok(None),
        }
    }
}

impl Drop for UnreadableFile {
    fn drop(&mut self) {
        if let Err(error) = fs::set_permissions(&self.path, self.permissions.clone()) {
            eprintln!(
                "cannot restore permissions of {}: {error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_are_restored_after_a_read_probe_or_unwinding() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let before = fs::metadata(file.path()).unwrap().permissions().mode();
        {
            let guard = UnreadableFile::new(file.path()).unwrap();
            if guard.is_some() {
                assert_eq!(
                    File::open(file.path()).unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
            }
        }
        assert_eq!(
            fs::metadata(file.path()).unwrap().permissions().mode(),
            before
        );
        let result = std::panic::catch_unwind(|| {
            let _guard = UnreadableFile::new(file.path()).unwrap();
            panic!("permission-test assertion failure");
        });
        assert!(result.is_err());
        assert_eq!(
            fs::metadata(file.path()).unwrap().permissions().mode(),
            before
        );
    }
}
