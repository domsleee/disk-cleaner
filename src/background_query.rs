//! Runs blocking queries away from rendering, keeping only the latest request.

use std::sync::mpsc;

pub struct BackgroundQuery<T> {
    receiver: Option<mpsc::Receiver<T>>,
    pending: Option<Box<dyn FnOnce() -> T + Send>>,
    discard_result: bool,
}

impl<T> Default for BackgroundQuery<T> {
    fn default() -> Self {
        Self {
            receiver: None,
            pending: None,
            discard_result: false,
        }
    }
}

impl<T: Send + 'static> BackgroundQuery<T> {
    pub fn is_active(&self) -> bool {
        self.receiver.is_some() || self.pending.is_some()
    }

    pub fn request(&mut self, query: impl FnOnce() -> T + Send + 'static) {
        // A stalled OS call cannot be interrupted. Keep one running query and
        // replace the queued request rather than spawning unbounded threads.
        self.pending = Some(Box::new(query));
        self.discard_result = true;
    }

    pub fn cancel(&mut self) {
        self.pending = None;
        self.discard_result = true;
    }

    pub fn poll(&mut self, ctx: &eframe::egui::Context) -> Option<T> {
        let mut result = None;
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(value) => {
                    self.receiver = None;
                    if !self.discard_result {
                        result = Some(value);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.receiver.is_none()
            && let Some(query) = self.pending.take()
        {
            let (sender, receiver) = mpsc::channel();
            self.receiver = Some(receiver);
            self.discard_result = false;
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = sender.send(query());
                ctx.request_repaint();
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Context;
    use std::time::{Duration, Instant};

    fn finish<T: Send + 'static>(query: &mut BackgroundQuery<T>, ctx: &Context) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = query.poll(ctx) {
                return value;
            }
            assert!(Instant::now() < deadline, "query did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn blocked_query_does_not_block_polling_or_start_more_workers() {
        let ctx = Context::default();
        let mut query = BackgroundQuery::default();
        let (release, blocked) = mpsc::channel();
        query.request(move || blocked.recv().unwrap());
        assert_eq!(query.poll(&ctx), None);
        // The worker cannot finish until this test releases it. Polling and
        // replacing requests must still return immediately.
        query.request(|| panic!("superseded request should never run"));
        let (started, latest) = mpsc::channel();
        query.request(move || {
            started.send(()).unwrap();
            3
        });
        assert_eq!(query.poll(&ctx), None);
        assert!(latest.try_recv().is_err(), "started a second worker");
        release.send(1).unwrap();
        assert_eq!(finish(&mut query, &ctx), 3);
        assert!(!query.is_active());
    }

    #[test]
    fn replacement_discards_a_result_already_waiting_in_the_channel() {
        let ctx = Context::default();
        let mut query = BackgroundQuery::default();
        let (sender, receiver) = mpsc::channel();
        sender.send(1).unwrap();
        query.receiver = Some(receiver);
        query.request(|| 2);
        assert_eq!(query.poll(&ctx), None);
        assert_eq!(finish(&mut query, &ctx), 2);
    }

    #[test]
    fn cancellation_discards_queued_and_completed_results() {
        let ctx = Context::default();
        let mut query = BackgroundQuery::default();
        let (sender, receiver) = mpsc::channel();
        sender.send(1).unwrap();
        query.receiver = Some(receiver);
        query.request(|| panic!("cancelled request should never run"));
        query.cancel();
        assert_eq!(query.poll(&ctx), None);
        assert!(!query.is_active());
        query.request(|| 2);
        assert_eq!(finish(&mut query, &ctx), 2);
    }

    #[test]
    fn slow_discovery_does_not_delay_an_independent_capacity_query() {
        let ctx = Context::default();
        let mut discovery = BackgroundQuery::default();
        let mut capacity = BackgroundQuery::default();
        let (release, blocked) = mpsc::channel();
        discovery.request(move || blocked.recv().unwrap());
        assert_eq!(discovery.poll(&ctx), None);
        capacity.request(|| Some((100, 40)));
        assert_eq!(finish(&mut capacity, &ctx), Some((100, 40)));
        assert!(discovery.is_active());
        release.send(()).unwrap();
        finish(&mut discovery, &ctx);
    }

    #[test]
    fn disconnected_worker_can_be_replaced() {
        let ctx = Context::default();
        let mut query = BackgroundQuery::default();
        let (sender, receiver) = mpsc::channel::<Option<u64>>();
        query.receiver = Some(receiver);
        drop(sender);
        query.request(|| None);
        // An unavailable disk is still a completed query, not a pending one.
        assert_eq!(finish(&mut query, &ctx), None);
        assert!(!query.is_active());
    }
}
