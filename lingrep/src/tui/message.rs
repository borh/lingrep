use std::sync::Arc;

use crate::tui::cache::{KwicLine, SurfaceEntry};
use crate::tui::model::PlaygroundMatch;
use crate::tui::paged_store::PagedStore;
use crate::tui::types::{ErrorCode, PatternId, RequestId, Tab, TaskKind};
use crate::tui::watcher::PatternFileChanged;
use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Debug)]
pub(crate) enum ExplorerMessage {
    Ui(UiMsg),

    PatternFileChanged(PatternFileChanged),

    PatternProgress {
        request_id: RequestId,
        current: usize,
        total: usize,
    },
    PatternDelta {
        request_id: RequestId,
        delta_counts: Vec<(String, u64)>,
    },
    PatternFinished {
        request_id: RequestId,
    },
    PatternFailed {
        request_id: RequestId,
        code: Option<ErrorCode>,
        error: String,
    },

    SurfaceFinished {
        request_id: RequestId,
        pattern_id: PatternId,
        store: Arc<PagedStore<SurfaceEntry>>,
    },
    SurfaceFailed {
        request_id: RequestId,
        pattern_id: PatternId,
        code: Option<ErrorCode>,
        error: String,
    },

    KwicFinished {
        request_id: RequestId,
        pattern_id: PatternId,
        store: Arc<PagedStore<KwicLine>>,
    },
    KwicFailed {
        request_id: RequestId,
        pattern_id: PatternId,
        code: Option<ErrorCode>,
        error: String,
    },
    PlaygroundComplete {
        request_id: RequestId,
        matches: Vec<PlaygroundMatch>,
    },
    PlaygroundFailed {
        request_id: RequestId,
        error: String,
    },

    Tick,
}

#[derive(Debug)]
pub enum UiMsg {
    KeyPress(KeyEvent),
    Mouse(MouseEvent),
    TabChange(Tab),
    SubmitPattern,
    StartEditPattern,
    SelectNextPattern,
    SelectPrevPattern,
    ConfirmSelection,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ToggleSort,
    ToggleDetailPanel,
    Cancel(TaskKind),
    DismissError,
    Exit,

    PlaygroundToggleFocus,
    SubmitPlayground,
}
