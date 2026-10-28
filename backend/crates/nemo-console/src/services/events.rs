//! SSE plumbing: connection cap, idle timeout, heartbeat, clean shutdown. Events are already redacted metadata.
//! Port of `services/events.py`. Contract (PORTING.md section 6.6): event names `: connected`, `: keep-alive`,
//! `event: idle-timeout`, `event: shutdown`, `event: error`, `data: <json>` and their exact bytes.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::Value;
use tokio::sync::watch;

use crate::adapters::base::AdapterRef;
use crate::services::common::RecoverLock as _;
use crate::services::common::redact_obj;
use crate::services::pyfmt::json_dumps;

pub struct SseLimiter {
    pub cap: usize,
    active: Mutex<usize>,
}

impl SseLimiter {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        SseLimiter {
            cap,
            active: Mutex::new(0),
        }
    }

    pub fn acquire(&self) -> bool {
        let Ok(mut a) = self.active.lock_recovering() else {
            return false;
        };
        if *a >= self.cap {
            return false;
        }
        *a += 1;
        true
    }

    pub fn release(&self) {
        if let Ok(mut a) = self.active.lock_recovering() {
            *a = a.saturating_sub(1);
        }
    }

    #[must_use]
    pub fn active(&self) -> usize {
        self.active.lock_recovering().map(|a| *a).unwrap_or(0)
    }
}

/// Server shutdown ends every open SSE stream cleanly (a final `event: shutdown`). Python keeps one such event per
/// event loop; Rust keeps one per app state.
#[derive(Clone)]
pub struct Shutdown {
    tx: Arc<watch::Sender<bool>>,
}

impl Default for Shutdown {
    fn default() -> Self {
        let (tx, _rx) = watch::channel(false);
        Shutdown { tx: Arc::new(tx) }
    }
}

impl Shutdown {
    /// `begin_shutdown()`: wake every open stream.
    pub fn begin(&self) {
        self.tx.send_replace(true);
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }

    #[must_use]
    pub fn is_set(&self) -> bool {
        *self.tx.borrow()
    }
}

struct Slot(Arc<SseLimiter>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.release();
    }
}

pub const CONNECTED: &[u8] = b": connected\n\n";
pub const KEEP_ALIVE: &[u8] = b": keep-alive\n\n";
pub const IDLE_TIMEOUT: &[u8] = b"event: idle-timeout\ndata: {}\n\n";
pub const SHUTDOWN: &[u8] = b"event: shutdown\ndata: {}\n\n";
pub const ERROR: &[u8] = b"event: error\ndata: {}\n\n";
pub const TOO_MANY: &[u8] = b"event: error\ndata: {\"error\": \"too many streams\"}\n\n";

/// The SSE body for one system. Takes its slot on the first poll and releases it when it ends or is dropped: a
/// stream that never starts holds nothing.
pub fn event_stream(
    a: AdapterRef,
    limiter: Arc<SseLimiter>,
    idle_timeout: f64,
    heartbeat: f64,
    shutdown: Shutdown,
) -> impl Stream<Item = Bytes> + Send + 'static {
    async_stream::stream! {
        if !limiter.acquire() {
            yield Bytes::from_static(TOO_MANY);
            return;
        }
        let _slot = Slot(Arc::clone(&limiter));
        let mut events = a.follow_events(None);
        let mut stop = shutdown.subscribe();
        let mut last_real = Instant::now();
        yield Bytes::from_static(CONNECTED);
        loop {
            if *stop.borrow() {
                yield Bytes::from_static(SHUTDOWN);
                return;
            }
            let remaining = idle_timeout - last_real.elapsed().as_secs_f64();
            if remaining <= 0.0 {
                yield Bytes::from_static(IDLE_TIMEOUT);
                return;
            }
            let wait = Duration::from_secs_f64(heartbeat.min(remaining).max(0.0));
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() {
                        // the sender is gone: nothing can wake us any more, keep serving until idle
                        tokio::time::sleep(wait).await;
                    }
                    continue;
                }
                () = tokio::time::sleep(wait) => {
                    yield Bytes::from_static(KEEP_ALIVE);
                    continue;
                }
                next = events.next() => {
                    match next {
                        None => return,
                        Some(Err(_)) => {
                            yield Bytes::from_static(ERROR);
                            return;
                        }
                        Some(Ok(ev)) => {
                            last_real = Instant::now();
                            let doc = serde_json::to_value(&ev).unwrap_or(Value::Null);
                            let payload = json_dumps(&redact_obj(&doc));
                            yield Bytes::from(format!("data: {payload}\n\n"));
                        }
                    }
                }
            }
        }
    }
}
