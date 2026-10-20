use std::hash::Hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PatternId(pub u32);

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    CacheError,
    PatternError,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMode {
    Viewing { tab: Tab },
    EditingPattern,
    Exiting,
    Error { message: String },
}

impl Default for AppMode {
    fn default() -> Self {
        Self::Viewing {
            tab: Tab::Frequencies,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Frequencies,
    Surface,
    Context,
    Parsed,
    Playground,
}

impl Tab {
    pub fn next(self) -> Self {
        match self {
            Tab::Frequencies => Tab::Surface,
            Tab::Surface => Tab::Context,
            Tab::Context => Tab::Parsed,
            Tab::Parsed => Tab::Playground,
            Tab::Playground => Tab::Frequencies,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Tab::Frequencies => Tab::Playground,
            Tab::Surface => Tab::Frequencies,
            Tab::Context => Tab::Surface,
            Tab::Parsed => Tab::Context,
            Tab::Playground => Tab::Parsed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaygroundFocus {
    #[default]
    Pattern,
    Sample,
}

impl PlaygroundFocus {
    pub fn toggle(self) -> Self {
        match self {
            PlaygroundFocus::Pattern => PlaygroundFocus::Sample,
            PlaygroundFocus::Sample => PlaygroundFocus::Pattern,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadStatus {
    #[default]
    Idle,
    Loading,
    Cancelling,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    pub current: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Pattern,
    Surface,
    Kwic,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_equality() {
        let id1 = RequestId(1);
        let id2 = RequestId(1);
        let id3 = RequestId(2);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_pattern_id_equality() {
        let id1 = PatternId(1);
        let id2 = PatternId(1);
        let id3 = PatternId(2);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_load_status_default() {
        let status = LoadStatus::default();
        assert_eq!(status, LoadStatus::Idle);
    }

    #[test]
    fn test_tab_cycle() {
        assert_eq!(Tab::Frequencies.next(), Tab::Surface);
        assert_eq!(Tab::Surface.next(), Tab::Context);
        assert_eq!(Tab::Context.next(), Tab::Parsed);
        assert_eq!(Tab::Parsed.next(), Tab::Playground);
        assert_eq!(Tab::Playground.next(), Tab::Frequencies);

        assert_eq!(Tab::Frequencies.prev(), Tab::Playground);
        assert_eq!(Tab::Surface.prev(), Tab::Frequencies);
        assert_eq!(Tab::Context.prev(), Tab::Surface);
        assert_eq!(Tab::Parsed.prev(), Tab::Context);
        assert_eq!(Tab::Playground.prev(), Tab::Parsed);
    }

    #[test]
    fn test_playground_focus_toggle() {
        let focus = PlaygroundFocus::Pattern;
        assert_eq!(focus.toggle(), PlaygroundFocus::Sample);
        assert_eq!(focus.toggle().toggle(), PlaygroundFocus::Pattern);
    }
}
