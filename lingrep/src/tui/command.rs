use crate::tui::types::{PatternId, RequestId, TaskKind};

#[derive(Debug)]
pub enum Command {
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

    Cancel {
        kind: TaskKind,
        request_id: RequestId,
    },
}
