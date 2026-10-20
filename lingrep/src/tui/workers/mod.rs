use crate::tui::types::RequestId;
use lingrep_core::execution::CancellationFlag;

mod kwic;
mod pattern;
mod surface;

pub(crate) use kwic::KwicWorker;
pub(crate) use pattern::PatternWorker;
pub(crate) use surface::SurfaceWorker;

pub(crate) struct TaskHandle {
    pub request_id: RequestId,
    pub cancel: CancellationFlag,
}

impl TaskHandle {
    fn spawn(request_id: RequestId, run: impl FnOnce(CancellationFlag) + Send + 'static) -> Self {
        let cancel = CancellationFlag::new();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || run(worker_cancel));
        Self { request_id, cancel }
    }
}

#[derive(Default)]
pub(crate) struct TaskHandles {
    pub pattern: Option<TaskHandle>,
    pub surface: Option<TaskHandle>,
    pub kwic: Option<TaskHandle>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_handles_default() {
        let handles = TaskHandles::default();
        assert!(handles.pattern.is_none());
        assert!(handles.surface.is_none());
        assert!(handles.kwic.is_none());
    }
}
