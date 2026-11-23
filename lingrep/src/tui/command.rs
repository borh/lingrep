use std::path::PathBuf;
use std::sync::Arc;

use crate::tui::cache::{KwicLine, SurfaceEntry};
use crate::tui::paged_store::PagedStore;
use crate::tui::types::{PatternId, RequestId, TaskKind};

#[derive(Debug)]
pub(crate) enum Command {
    StartPattern {
        request_id: RequestId,
        pattern_text: String,
    },
    StartSurface {
        request_id: RequestId,
        pattern_id: PatternId,
        pattern_name: String,
        pattern_text: String,
    },
    StartKwic {
        request_id: RequestId,
        pattern_id: PatternId,
        pattern_name: String,
        pattern_text: String,
    },
    StartPlayground {
        request_id: RequestId,
        pattern_text: String,
        sample_text: String,
    },
    ReloadPatternFile {
        path: PathBuf,
        reload_count: u64,
    },
    LoadSurfaceWindow {
        request_id: RequestId,
        pattern_id: PatternId,
        page_request_id: u64,
        store: Arc<PagedStore<SurfaceEntry>>,
        start: usize,
    },
    LoadKwicWindow {
        request_id: RequestId,
        pattern_id: PatternId,
        page_request_id: u64,
        store: Arc<PagedStore<KwicLine>>,
        start: usize,
    },

    Cancel {
        kind: TaskKind,
        request_id: RequestId,
    },
}
