use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct PatternWatcher {
    path: PathBuf,
    last_modified: Option<SystemTime>,
    reload_count: u64,
}

impl PatternWatcher {
    pub fn new(path: PathBuf) -> Self {
        let last_modified = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());

        Self {
            path,
            last_modified,
            reload_count: 0,
        }
    }

    pub fn check(&mut self) -> Option<PatternFileChanged> {
        let current_modified = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| m.modified().ok());

        match (self.last_modified, current_modified) {
            (Some(last), Some(current)) if current > last => {
                self.last_modified = Some(current);
                self.reload_count += 1;
                Some(PatternFileChanged {
                    path: self.path.clone(),
                    reload_count: self.reload_count,
                })
            }
            (None, Some(current)) => {
                self.last_modified = Some(current);
                self.reload_count += 1;
                Some(PatternFileChanged {
                    path: self.path.clone(),
                    reload_count: self.reload_count,
                })
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn reload_count(&self) -> u64 {
        self.reload_count
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatternFileChanged {
    pub path: PathBuf,
    pub reload_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_watcher_detects_new_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.lx");

        let mut watcher = PatternWatcher::new(path.clone());
        assert_eq!(watcher.reload_count(), 0);

        fs::write(&path, "pattern test := [upos=VERB].").unwrap();
        thread::sleep(Duration::from_millis(10));

        let changed = watcher.check();
        assert!(changed.is_some());
        assert_eq!(watcher.reload_count(), 1);
    }

    #[test]
    fn test_watcher_detects_modification() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.lx");

        fs::write(&path, "initial").unwrap();
        thread::sleep(Duration::from_millis(10));

        let mut watcher = PatternWatcher::new(path.clone());
        assert_eq!(watcher.check(), None);

        thread::sleep(Duration::from_millis(10));
        let mut file = File::options().write(true).open(&path).unwrap();
        file.write_all(b"modified").unwrap();
        drop(file);

        thread::sleep(Duration::from_millis(10));

        let changed = watcher.check();
        assert!(changed.is_some());
        assert_eq!(watcher.reload_count(), 1);
    }

    #[test]
    fn test_watcher_no_change_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.lx");

        fs::write(&path, "pattern test := [upos=VERB].").unwrap();
        thread::sleep(Duration::from_millis(10));

        let mut watcher = PatternWatcher::new(path.clone());

        // First check should return None since no change since creation
        assert!(watcher.check().is_none());
        assert!(watcher.check().is_none());
    }
}
