//! The latest live snapshot per run, for a client that starts watching
//! mid-run (seam-contract D14's amendment, task 048 Scope 5).
//!
//! A stream that opens after a run began has missed every `runs:tail` so far.
//! What it reads once to catch up is the newest snapshot, which `get_run_tail`
//! answers from here. Not a scrollback: the transcript is that (ADR-0013), and
//! the snapshot is already the whole live view ADR-0013 specifies.
//!
//! **One follower of the live channel, never written to directly.** Every tail
//! enters through [`ServiceContext::publish_tail`](crate::ServiceContext::publish_tail),
//! and this relay hears it as every open stream does. A writer that fed the
//! relay itself would be a tail no stream heard, or one the relay missed.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::events::{RunId, TeamTail};

/// The relay's capacity on the solo desktop: task 008's
/// `MAX_TRACKED_RUN_TAILS`, which one machine's runs never come near.
pub const SOLO_TAIL_RELAY_CAPACITY: usize = 32;

/// The relay's capacity on a server, which relays every team's runners
/// (ADR-0036 point 2). A snapshot that ages out costs a client opening that
/// run mid-flight only the wait for its next snapshot.
pub const SERVER_TAIL_RELAY_CAPACITY: usize = 1024;

/// The latest [`TeamTail`] per run, in a cache that forgets the run it heard
/// from least recently once it holds `capacity` runs.
///
/// Cheap to clone; every clone reads the same cache. `BoardHost` holds one,
/// and the host spawns [`follow`](Self::follow) once, before anything can
/// publish.
#[derive(Clone)]
pub struct TailRelay {
    inner: Arc<Mutex<Cache>>,
}

struct Cache {
    capacity: usize,
    /// First heard, oldest first, for eviction.
    order: VecDeque<RunId>,
    latest: HashMap<RunId, TeamTail>,
}

impl TailRelay {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Cache {
                capacity: capacity.max(1),
                order: VecDeque::new(),
                latest: HashMap::new(),
            })),
        }
    }

    /// Records every snapshot `tails` delivers until the channel closes.
    ///
    /// Takes a receiver rather than a context, so the subscription exists the
    /// moment the host calls this, before the future is spawned: a tail
    /// published in between is not lost. A lagged receiver drops what it
    /// missed and carries on (D14 rule 1): the next snapshot of each run
    /// replaces it anyway.
    pub fn follow(
        &self,
        mut tails: broadcast::Receiver<TeamTail>,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let relay = self.clone();
        async move {
            loop {
                match tails.recv().await {
                    Ok(tail) => relay.record(tail),
                    Err(RecvError::Lagged(dropped)) => {
                        tracing::debug!(dropped, "the tail relay fell behind the live channel");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }

    /// The newest snapshot of `run_id`, with its team, or `None` when none has
    /// been heard or it aged out.
    pub fn latest(&self, run_id: &str) -> Option<TeamTail> {
        self.lock().latest.get(run_id).cloned()
    }

    fn record(&self, tail: TeamTail) {
        let mut cache = self.lock();
        let run_id = tail.tail.run_id.clone();
        if !cache.latest.contains_key(&run_id) {
            cache.order.push_back(run_id.clone());
            if cache.order.len() > cache.capacity {
                if let Some(oldest) = cache.order.pop_front() {
                    cache.latest.remove(&oldest);
                }
            }
        }
        cache.latest.insert(run_id, tail);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.inner.lock().expect("the tail relay is poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::events::RunTail;
    use pretty_assertions::assert_eq;

    const TEAM: &str = "3f2b1c00-0000-4000-8000-0000000000a1";

    fn tail(run_id: &str, elapsed_ms: i64) -> TeamTail {
        TeamTail {
            team_id: TEAM.to_string(),
            tail: RunTail {
                run_id: run_id.to_string(),
                elapsed_ms,
                turns: 0,
                current_tool: None,
                last_assistant_text: None,
            },
        }
    }

    /// Feeds `tails` through a real channel to a follower, and waits for the
    /// follower to finish: closing the channel is what ends it, so every
    /// snapshot has been recorded when this returns, with no timer.
    async fn relayed(capacity: usize, tails: Vec<TeamTail>) -> TailRelay {
        let relay = TailRelay::new(capacity);
        let (sender, receiver) = broadcast::channel(tails.len().max(1));
        let follower = relay.follow(receiver);
        for tail in tails {
            sender.send(tail).expect("the follower is subscribed");
        }
        drop(sender);
        follower.await;
        relay
    }

    #[tokio::test]
    async fn the_tail_relay_keeps_the_latest_snapshot_per_run_and_forgets_the_oldest() {
        let relay = relayed(
            2,
            vec![
                tail("first", 1),
                tail("second", 1),
                tail("first", 2),
                tail("third", 1),
            ],
        )
        .await;

        // `first` was heard first, so the third run evicts it, however
        // recently it spoke: the order is of first hearing.
        assert_eq!(relay.latest("first"), None);
        assert_eq!(relay.latest("second"), Some(tail("second", 1)));
        assert_eq!(relay.latest("third"), Some(tail("third", 1)));
    }

    #[tokio::test]
    async fn a_newer_snapshot_replaces_the_older_one_for_its_run() {
        let relay = relayed(2, vec![tail("run", 1), tail("run", 5)]).await;

        assert_eq!(relay.latest("run"), Some(tail("run", 5)));
        assert_eq!(relay.latest("never-heard"), None);
    }
}
