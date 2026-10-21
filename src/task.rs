use std::{sync::mpsc, thread};

use anyhow::anyhow;
use eframe::egui;

/// Work on a background thread whose result the UI picks up in a later frame.
pub struct Task<T> {
    receiver: mpsc::Receiver<anyhow::Result<T>>,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(
        ctx: &egui::Context,
        work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            sender.send(work()).ok();
            ctx.request_repaint();
        });
        Self { receiver }
    }

    /// The result, once. Drop the task after it returns `Some`.
    pub fn poll(&self) -> Option<anyhow::Result<T>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow!("The background task crashed")))
            }
        }
    }
}
