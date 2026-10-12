//! A cancellable background job with progress, polled from the UI thread.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub enum TaskState<T> {
    Running { progress: f32, stage: String },
    Done(T),
    Failed(String),
    Cancelled,
}

pub struct Task<T> {
    state: Arc<Mutex<TaskState<T>>>,
    cancel: Arc<AtomicBool>,
}

impl<T: Send + 'static> Task<T> {
    /// Runs `job` on a new thread. The job reports progress through its callback and
    /// returns `Ok(None)` when it noticed the cancel flag.
    pub fn spawn<F>(ctx: egui::Context, job: F) -> Self
    where
        F: FnOnce(&AtomicBool, &mut dyn FnMut(f32, &str)) -> Result<Option<T>> + Send + 'static,
    {
        let state = Arc::new(Mutex::new(TaskState::Running { progress: 0.0, stage: "Starting…".into() }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn) = (state.clone(), cancel.clone());
        std::thread::spawn(move || {
            let progress_state = st.clone();
            let progress_ctx = ctx.clone();
            let mut report = move |p: f32, stage: &str| {
                *progress_state.lock().unwrap() = TaskState::Running { progress: p, stage: stage.to_string() };
                progress_ctx.request_repaint();
            };
            let result = job(&cn, &mut report);
            *st.lock().unwrap() = match result {
                Ok(Some(v)) => TaskState::Done(v),
                Ok(None) => TaskState::Cancelled,
                Err(_) if cn.load(Ordering::Relaxed) => TaskState::Cancelled,
                Err(e) => TaskState::Failed(format!("{e:#}")),
            };
            ctx.request_repaint();
        });
        Self { state, cancel }
    }

    /// Progress while running, `None` once finished.
    pub fn progress(&self) -> Option<(f32, String)> {
        match &*self.state.lock().unwrap() {
            TaskState::Running { progress, stage } => Some((*progress, stage.clone())),
            _ => None,
        }
    }

    /// Takes the final state once the job has finished.
    pub fn take_finished(&self) -> Option<TaskState<T>> {
        let mut s = self.state.lock().unwrap();
        if matches!(*s, TaskState::Running { .. }) {
            return None;
        }
        Some(std::mem::replace(&mut *s, TaskState::Cancelled))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
