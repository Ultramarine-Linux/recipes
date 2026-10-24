//! Coalescing build queue: at most one build runs, and at most one waits.
//!
//! Requests made while a build runs collapse into a single pending slot that
//! holds the newest requested commit, so a burst of pushes costs at most two
//! builds and the last push always wins.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};

use tokio::sync::watch;

type Job = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Slots {
    pub running: Option<String>,
    pub pending: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requested {
    /// Nothing was running; the build started.
    Started,
    /// Another build is running; this one runs next (replacing any older
    /// pending request).
    Queued,
    /// That commit is already building.
    Running,
}

#[derive(Clone)]
pub struct BuildQueue {
    slots: Arc<Mutex<Slots>>,
    job: Job,
    busy: Arc<watch::Sender<bool>>,
}

impl BuildQueue {
    /// `job` builds one commit; it handles and records its own errors.
    pub fn new<F, Fut>(job: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self {
            slots: Arc::default(),
            job: Arc::new(move |sha| Box::pin(job(sha))),
            busy: Arc::new(watch::Sender::new(false)),
        }
    }

    /// Must be called within a Tokio runtime.
    pub fn request(&self, sha: String) -> Requested {
        let mut slots = self.lock();
        match &slots.running {
            None => {
                slots.running = Some(sha);
                self.busy.send_replace(true);
                tokio::spawn(self.clone().drain());
                Requested::Started
            }
            // The newest request is what's building: drop an older pending one.
            Some(running) if *running == sha => {
                slots.pending = None;
                Requested::Running
            }
            Some(_) => {
                slots.pending = Some(sha);
                Requested::Queued
            }
        }
    }

    #[must_use]
    pub fn slots(&self) -> Slots {
        self.lock().clone()
    }

    /// Resolves once nothing is running or pending.
    pub async fn idle(&self) {
        let mut busy = self.busy.subscribe();
        // Only fails if the sender is gone, and `self` holds it.
        let _idle = busy.wait_for(|busy| !busy).await;
    }

    async fn drain(self) {
        loop {
            let Some(sha) = self.lock().running.clone() else {
                break;
            };
            // Run each build in its own task, so a panic fails that build
            // instead of wedging the queue.
            if let Err(error) = tokio::spawn((self.job)(sha.clone())).await {
                tracing::error!("build {sha} panicked: {error}");
            }
            let mut slots = self.lock();
            slots.running = slots.pending.take();
            if slots.running.is_none() {
                self.busy.send_replace(false);
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slots> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::{Semaphore, mpsc};

    use super::*;

    /// A queue whose builds block until a permit is added, reporting each
    /// started build.
    fn gated() -> (BuildQueue, Arc<Semaphore>, mpsc::UnboundedReceiver<String>) {
        let gate = Arc::new(Semaphore::new(0));
        let (started, receiver) = mpsc::unbounded_channel();
        let queue = BuildQueue::new({
            let gate = Arc::clone(&gate);
            move |sha| {
                let gate = Arc::clone(&gate);
                let started = started.clone();
                async move {
                    started.send(sha).unwrap();
                    gate.acquire().await.unwrap().forget();
                }
            }
        });
        (queue, gate, receiver)
    }

    #[tokio::test]
    async fn coalesces_to_the_newest_commit() {
        let (queue, gate, mut started) = gated();
        assert_eq!(queue.request("a".into()), Requested::Started);
        assert_eq!(started.recv().await.unwrap(), "a");
        assert_eq!(queue.request("b".into()), Requested::Queued);
        assert_eq!(queue.request("c".into()), Requested::Queued);
        assert_eq!(queue.request("a".into()), Requested::Running);
        assert_eq!(
            queue.slots(),
            Slots {
                running: Some("a".into()),
                pending: None
            }
        );
        assert_eq!(queue.request("d".into()), Requested::Queued);
        assert_eq!(queue.slots().pending.as_deref(), Some("d"));

        gate.add_permits(1);
        assert_eq!(started.recv().await.unwrap(), "d");
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), queue.idle())
            .await
            .unwrap();
        assert_eq!(queue.slots(), Slots::default());
        // b and c were never built.
        assert!(started.try_recv().is_err());

        assert_eq!(queue.request("e".into()), Requested::Started);
        assert_eq!(started.recv().await.unwrap(), "e");
        gate.add_permits(1);
        queue.idle().await;
    }

    #[tokio::test]
    async fn survives_a_panicking_build() {
        let queue = BuildQueue::new(|sha: String| async move {
            assert_ne!(sha, "boom", "build failed");
        });
        queue.request("boom".into());
        queue.idle().await;
        assert_eq!(queue.request("ok".into()), Requested::Started);
        queue.idle().await;
        assert_eq!(queue.slots(), Slots::default());
    }
}
