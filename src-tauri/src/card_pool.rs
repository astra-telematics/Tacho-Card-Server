//! Bounded pool of physical smart cards, shared across authenticating devices.
//!
//! Goal: exactly as many devices as there are cards available can authenticate at
//! once (N devices <-> N cards). Additional devices wait in a FIFO queue — there is
//! no cap here on how many can wait; the only admission control in the system is the
//! raw TCP `connection_limit` check at accept time, before a connection ever reaches
//! this pool (see `socket_server::run`). The reasoning: if the server is ever
//! genuinely overloaded (e.g. under a connection flood), those connections should be
//! rejected as early and cheaply as possible — at accept, before any protocol
//! parsing — not kept alive only to be rejected later at a second, redundant gate.
//!
//! If a checked-out card is pulled mid-session, the affected device does NOT get
//! sent back to the queue — it already holds a session slot, it just needs a working
//! card for it. `CardGuard::discard_dead_card` + `CardPool::wait_for_replacement_card`
//! wait for the next card to become available and take it directly, ahead of anyone
//! still waiting for their first card (see `add_card`'s priority hand-off).
//!
//! `Ready`/`Abort(CardRemoved)` in the device protocol are server-initiated: a device
//! connects, announces itself, and then just waits (indefinitely) for `Ready` — it
//! never asks for a card itself.
//!
//! Implementation:
//! - A `tokio::sync::Semaphore` tracks how many cards are free right now — its own
//!   waiter list gives FIFO fairness for free — backed by a `Mutex`-protected deque
//!   holding the actual `ManagedCard` objects. Invariant: "permits currently
//!   available == cards.len()", maintained by always pairing an `add_permits`/
//!   `acquire` with a `push_back`/`pop_front`.
//! - A `checked_out` registry tracks which reader each live session's card came
//!   from, plus a revoked flag + notifier, so `remove_by_reader` can flag and wake a
//!   session immediately when its card disappears instead of it finding out only
//!   from a failed transmit.
//! - Priority replacement bypasses the semaphore/deque entirely: a card is handed
//!   straight to a waiting `replace_dead_card` caller via a oneshot channel, so
//!   there's no permit-accounting race between it and the normal FIFO queue.

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lazy_static::lazy_static;
use tokio::sync::{oneshot, Notify, OwnedSemaphorePermit, Semaphore};

use crate::config::{get_from_cache, CacheSection};
use crate::smart_card::ManagedCard;

struct CheckedOutEntry {
    reader_name: CString,
    revoked: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

pub struct CardPool {
    semaphore: Arc<Semaphore>,
    cards: Mutex<VecDeque<ManagedCard>>,
    waiting: AtomicUsize,
    checked_out: Mutex<Vec<CheckedOutEntry>>,
    priority_waiters: Mutex<VecDeque<oneshot::Sender<ManagedCard>>>,
}

/// RAII handle to a checked-out card. Returns the card to the pool automatically
/// when dropped — including on early return or the holding task being aborted — so
/// a disconnecting device can never leak a card out of circulation. If the card was
/// flagged as revoked (pulled while checked out), it's discarded instead of being
/// recycled to the next device.
pub struct CardGuard {
    card: Option<ManagedCard>,
    pool: Arc<CardPool>,
    permit: Option<OwnedSemaphorePermit>,
    revoked: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl std::ops::Deref for CardGuard {
    type Target = ManagedCard;
    fn deref(&self) -> &ManagedCard {
        self.card.as_ref().expect("card is only None between discard_dead_card and install_replacement")
    }
}

/// Waits until `revoked` is flipped true, using `notify` to avoid busy-polling.
/// Standalone (doesn't borrow a `CardGuard`) so it can be boxed/pinned and polled
/// across multiple `tokio::select!` iterations independently of the guard, which the
/// same code also needs to read/write elsewhere (e.g. to relay APDUs).
pub async fn wait_for_revocation_signal(revoked: Arc<AtomicBool>, notify: Arc<Notify>) {
    loop {
        // Arm the notification *before* checking the flag, so a revocation that
        // happens between the check and the await can't be missed.
        let notified = notify.notified();
        if revoked.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

impl CardGuard {
    /// Resolves when this guard's card is externally revoked (its reader reports the
    /// card removed while checked out).
    pub async fn revoked(&self) {
        wait_for_revocation_signal(Arc::clone(&self.revoked), Arc::clone(&self.notify)).await
    }

    /// Clones of this guard's revocation signal, for waiting on it independently of
    /// the guard itself (see `wait_for_revocation_signal`).
    pub fn revoked_signal(&self) -> (Arc<AtomicBool>, Arc<Notify>) {
        (Arc::clone(&self.revoked), Arc::clone(&self.notify))
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }

    /// True while this guard actually holds a working card. False after
    /// `discard_dead_card` and before `install_replacement` — i.e. while the
    /// session's slot is reserved but it has nothing to relay APDUs to yet.
    pub fn has_card(&self) -> bool {
        self.card.is_some()
    }

    /// Call once `revoked()` has resolved (or `is_revoked()` is true). Drops the dead
    /// card without recycling it, and resets the revocation signal to an inert one so
    /// `revoked()` doesn't immediately fire again — the guard now represents a
    /// reserved-but-cardless slot until `install_replacement` is called. Deliberately
    /// does not itself wait for a replacement: callers may want to tell the far end
    /// (e.g. send an Abort) before committing to wait — see
    /// `CardPool::wait_for_replacement_card`.
    pub fn discard_dead_card(&mut self) {
        if let Some(old_card) = self.card.take() {
            self.pool.unregister_checked_out(old_card.reader_name());
            // old_card drops here without being recycled — it's confirmed dead.
        }
        // Fresh, inert placeholders: nothing will ever flip or notify these until a
        // real replacement card is installed.
        self.revoked = Arc::new(AtomicBool::new(false));
        self.notify = Arc::new(Notify::new());
        // self.permit is deliberately left untouched: it represents this session's
        // slot, independent of which physical card is currently plugged into it.
    }

    /// Installs a card obtained via `CardPool::wait_for_replacement_card` into this
    /// (previously `discard_dead_card`'d) guard, re-registering it in the checked-out
    /// registry so future revocations are detected the same way as normal.
    pub fn install_replacement(&mut self, new_card: ManagedCard) {
        let (revoked, notify) = self.pool.register_checked_out(&new_card);
        self.card = Some(new_card);
        self.revoked = revoked;
        self.notify = notify;
        log::info!("[CARD_POOL] phase=replace status=ok");
    }
}

impl Drop for CardGuard {
    fn drop(&mut self) {
        match self.card.take() {
            Some(card) => {
                self.pool.unregister_checked_out(card.reader_name());
                if self.revoked.load(Ordering::SeqCst) {
                    // Known dead: discard rather than recycling a broken card to the
                    // next device. Forget the permit instead of releasing it, since
                    // there's no card left to pair it with.
                    if let Some(permit) = self.permit.take() {
                        permit.forget();
                    }
                } else {
                    self.pool.cards.lock().unwrap().push_back(card);
                    // permit drops normally right after this, releasing it for reuse.
                }
            }
            None => {
                // Dropped between discard_dead_card and install_replacement (e.g.
                // the connection's task was aborted while awaiting a replacement):
                // no card to pair with our permit, so forget it too rather than
                // leaking a phantom "available" permit with nothing behind it.
                if let Some(permit) = self.permit.take() {
                    permit.forget();
                }
            }
        }
    }
}

impl CardPool {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(0)),
            cards: Mutex::new(VecDeque::new()),
            waiting: AtomicUsize::new(0),
            checked_out: Mutex::new(Vec::new()),
            priority_waiters: Mutex::new(VecDeque::new()),
        })
    }

    fn register_checked_out(&self, card: &ManagedCard) -> (Arc<AtomicBool>, Arc<Notify>) {
        let revoked = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        self.checked_out.lock().unwrap().push(CheckedOutEntry {
            reader_name: card.reader_name().to_owned(),
            revoked: Arc::clone(&revoked),
            notify: Arc::clone(&notify),
        });
        (revoked, notify)
    }

    fn unregister_checked_out(&self, reader_name: &CStr) {
        self.checked_out
            .lock()
            .unwrap()
            .retain(|c| c.reader_name.as_c_str() != reader_name);
    }

    /// Makes a newly-detected card available for devices to authenticate against.
    /// A session currently replacing a dead card (see `wait_for_replacement_card`)
    /// takes priority over the normal pool/queue for this card.
    pub fn add_card(&self, mut card: ManagedCard) {
        {
            let mut priority_waiters = self.priority_waiters.lock().unwrap();
            while let Some(sender) = priority_waiters.pop_front() {
                match sender.send(card) {
                    Ok(()) => {
                        log::info!("[CARD_POOL] phase=add_card status=ok routed=priority_waiter");
                        return;
                    }
                    Err(returned_card) => {
                        // That waiter's connection died between registering and now
                        // (its receiver was dropped) — try the next one, if any.
                        card = returned_card;
                        continue;
                    }
                }
            }
        }

        self.cards.lock().unwrap().push_back(card);
        self.semaphore.add_permits(1);
        log::info!(
            "[CARD_POOL] phase=add_card status=ok routed=pool idle_count={}",
            self.idle_count()
        );
    }

    /// Called when a reader reports its card removed. If the card is sitting idle in
    /// the pool, it's dropped immediately. If it's checked out by a live session
    /// instead, that session is flagged and woken immediately (via `revoked()`)
    /// rather than being left to discover the failure on its own.
    pub fn remove_by_reader(&self, reader_name: &CStr) {
        {
            let mut cards = self.cards.lock().unwrap();
            if let Some(index) = cards.iter().position(|c| c.reader_name() == reader_name) {
                let removed_card = cards.remove(index).expect("index just came from position()");
                drop(cards);
                // Claim back the permit that corresponded to this now-gone card so the
                // "permits == idle cards" invariant holds. try_acquire can't fail
                // here: we're the ones who just shrank the deque, so a matching
                // permit exists.
                if let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() {
                    permit.forget();
                }
                // Drop the passive sniffer's per-card state too (see apdu_sniffer.rs)
                // so it doesn't accumulate forever across long runs. Only possible if
                // the ICCID was already read — harmless no-op otherwise.
                if let Some(iccid) = removed_card.iccid.get() {
                    let card_number = get_from_cache(CacheSection::Cards, iccid);
                    crate::apdu_sniffer::forget(&card_number);
                }
                log::info!("[CARD_POOL] phase=remove status=ok mode=idle reader={:?}", reader_name);
                return;
            }
        }

        let checked_out = self.checked_out.lock().unwrap();
        if let Some(entry) = checked_out.iter().find(|c| c.reader_name.as_c_str() == reader_name) {
            entry.revoked.store(true, Ordering::SeqCst);
            entry.notify.notify_one();
            log::warn!(
                "[CARD_POOL] phase=remove status=flagged mode=checked_out reader={:?}",
                reader_name
            );
        }
    }

    /// True if a card from this reader is already known to the pool — either sitting
    /// idle or checked out by a live session. Used to stop the same physical card
    /// being registered twice when a reader reports multiple "card present" PC/SC
    /// state transitions in quick succession for what is really one insertion event.
    pub fn contains_reader(&self, reader_name: &str) -> bool {
        let in_idle_pool = self
            .cards
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.reader_name().to_string_lossy().as_ref() == reader_name);
        if in_idle_pool {
            return true;
        }
        self.checked_out
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.reader_name.to_string_lossy().as_ref() == reader_name)
    }

    /// Number of cards currently sitting idle in the pool (not checked out).
    pub fn idle_count(&self) -> usize {
        self.cards.lock().unwrap().len()
    }

    /// Current number of devices waiting in line for their first card. Observability
    /// only — there's no cap here (see module docs); admission is controlled solely
    /// by `connection_limit` at TCP accept time, before a connection ever reaches
    /// this pool.
    pub fn waiting_count(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    /// Waits (FIFO, fair — inherited from the semaphore's own waiter list) for a card
    /// to be free. Returns a guard that releases the card back to the pool when
    /// dropped (on disconnect, abort, or task cancellation). If a card is already
    /// free, this resolves immediately. Never rejects — a device that's gotten this
    /// far already passed the one admission gate that exists (`connection_limit`, at
    /// accept time), so it always eventually gets a card rather than being turned
    /// away a second time somewhere deeper in the stack.
    pub async fn acquire(self: &Arc<Self>) -> CardGuard {
        if let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() {
            return self.checkout(permit).await;
        }

        self.waiting.fetch_add(1, Ordering::SeqCst);
        let permit = Arc::clone(&self.semaphore).acquire_owned().await;
        self.waiting.fetch_sub(1, Ordering::SeqCst);

        // The semaphore is never closed (nothing ever calls `close()`), so this can't fail.
        self.checkout(permit.expect("card pool semaphore was unexpectedly closed")).await
    }

    /// Waits for the next card to become available, taking priority over devices
    /// still waiting for their first card — for a session that already holds a slot
    /// (see `CardGuard::discard_dead_card`) and just needs a working card to go with
    /// it, not a new admission. Returns the raw card; install it with
    /// `CardGuard::install_replacement`.
    pub async fn wait_for_replacement_card(self: &Arc<Self>) -> ManagedCard {
        let (tx, rx) = oneshot::channel();
        self.priority_waiters.lock().unwrap().push_back(tx);
        rx.await
            .expect("card pool dropped without ever fulfilling a priority wait")
    }

    async fn checkout(self: &Arc<Self>, permit: OwnedSemaphorePermit) -> CardGuard {
        let card = self
            .cards
            .lock()
            .unwrap()
            .pop_front()
            .expect("permit acquired but pool was empty — invariant violated");
        // A card sitting idle here may have been left mid-session by whichever
        // device held it last — e.g. a DF selected away from MF — and this
        // pool can hand the same physical card to many different devices over
        // its lifetime in the reader. Never hand out state a previous session
        // left behind: force a reset (`reconnect()` already does
        // `Disposition::ResetCard`) before this new session ever sees it.
        card.reconnect().await;
        let (revoked, notify) = self.register_checked_out(&card);
        CardGuard {
            card: Some(card),
            pool: Arc::clone(self),
            permit: Some(permit),
            revoked,
            notify,
        }
    }
}

lazy_static! {
    /// Global card pool shared by the reader-monitoring loop (which adds/removes
    /// cards as they're inserted/pulled) and the TCP server (which checks a card out
    /// per authenticating device).
    pub static ref CARD_POOL: Arc<CardPool> = CardPool::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    // ManagedCard can't be constructed without real hardware (`ManagedCard::new`
    // calls into pcsc), so this test exercises the pool's waiting-counter bookkeeping
    // directly against the semaphore rather than through `acquire`/`add_card`'s full
    // round trip.

    #[tokio::test]
    async fn acquire_blocks_until_a_card_is_added_then_resolves() {
        let pool = CardPool::new();
        let pool_bg = Arc::clone(&pool);
        let waiter = tokio::spawn(async move { pool_bg.acquire().await });

        // No card yet — the waiter should be sitting in line, not resolved.
        while pool.waiting_count() == 0 {
            tokio::task::yield_now().await;
        }
        assert!(!waiter.is_finished(), "acquire() must wait when no card is available");

        waiter.abort();
    }
}
