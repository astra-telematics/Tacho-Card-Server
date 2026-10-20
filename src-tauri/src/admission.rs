//! Busyness estimate, and when to tell a turned-away device to come back.
//!
//! Everything here is kept per company (each has its own cards and queue), plus once
//! more across all companies for connections turned away before they've announced —
//! and so before their company is known.
//!
//! - **Average card session.** How long a device holds a card, from `Ready` until the
//!   card is released (normally at `AuthOutcome`, i.e. its authentication time).
//!   Averaged over the last `SESSION_SAMPLES` sessions and persisted in the database so
//!   it survives restarts. A company with no history yet uses the all-company average,
//!   then `DEFAULT_CARD_SESSION`.
//! - **Queue estimate.** With `c` cards and `q` devices already waiting, a new device
//!   would wait roughly `(q + 1) × average ÷ c` for a card (zero if a card is free and
//!   nobody is waiting). Informational for admitted devices — anyone who announces
//!   queues, however long the wait.
//! - **Backlog timer.** A rough "busy until" time for connections turned away over a
//!   connection limit (global, or their company's), which we expect back. Each one pushes it out by
//!   `BACKLOG_SHARE_FACTOR` of its share of the cards (`average ÷ c`), and it runs down
//!   in real time. We can't tell when a turned-away connection comes back (over the
//!   global limit it hasn't even said who it is), so its share simply expires with
//!   the timer. The guide may read a little high just after a
//!   burst of rejections, which is fine for its purpose.
//!
//! A connection turned away over the limit is told to retry once a card should be
//! free: the later of the queue estimate and the backlog timer. Only a guide — devices
//! come back when they can (they may be asleep, or need ignition on).
//!
//! Devices turned away for the download interval are told to come back just after
//! midnight on the day they're allowed, spread over `MIDNIGHT_SPREAD_SECONDS`. They
//! don't count towards the backlog: that's days away and says nothing about now.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local};
use lazy_static::lazy_static;
use rand::Rng;

/// How many recent card sessions the average is taken over.
const SESSION_SAMPLES: usize = 100;
/// Assumed card session length until any real session has been recorded (about what
/// an authentication took in testing).
const DEFAULT_CARD_SESSION: Duration = Duration::from_secs(30);
/// Retry delay for a device turned away because its company has no cards at all, so
/// no wait can be estimated.
pub const NO_CARDS_RETRY: Duration = Duration::from_secs(5 * 60);
/// Devices told to come back at midnight are spread over this window after it, so
/// they don't all reconnect at 00:00:00.
const MIDNIGHT_SPREAD_SECONDS: u64 = 15 * 60;
/// Fraction of a turned-away device's share of the cards (`average ÷ cards`) that it
/// adds to the backlog timer. Below 1 to err on the side of underestimating: not every
/// turned-away device comes back when told.
const BACKLOG_SHARE_FACTOR: f64 = 0.8;
/// How long after its suggested retry time a turned-away connection still counts as
/// "expected back" in `Busyness::expected_returns`.
const EXPECTED_RETURN_GRACE: Duration = Duration::from_secs(60 * 60);

/// The card pool's state at one moment, plus the current average session length.
#[derive(Clone, Copy, Debug)]
pub struct QueueSnapshot {
    pub cards: usize,
    pub idle_cards: usize,
    pub waiting: usize,
    pub average_session: Duration,
}

impl QueueSnapshot {
    /// Roughly how long a device announcing now would wait for a card. `None` if there
    /// are no cards, so it would wait indefinitely.
    pub fn estimated_wait(&self) -> Option<Duration> {
        if self.cards == 0 {
            return None;
        }
        if self.idle_cards > 0 && self.waiting == 0 {
            return Some(Duration::ZERO);
        }
        Some(self.average_session.mul_f64((self.waiting + 1) as f64 / self.cards as f64))
    }

    /// How much one turned-away device adds to the backlog: `BACKLOG_SHARE_FACTOR` of
    /// its share of the cards' time.
    fn backlog_share(&self) -> Duration {
        (self.average_session / self.cards.max(1) as u32).mul_f64(BACKLOG_SHARE_FACTOR)
    }
}

/// Connections turned away over `connection_limit` that we expect back, and the
/// resulting "busy until" time.
#[derive(Default)]
struct Backlog {
    /// When the cards should have got through everyone we've turned away. `None` or in
    /// the past means no backlog.
    busy_until: Option<DateTime<Local>>,
    /// Suggested retry time of each turned-away connection, oldest first.
    expected_returns: VecDeque<DateTime<Local>>,
}

impl Backlog {
    fn remaining(&self, now: DateTime<Local>) -> Duration {
        self.busy_until
            .and_then(|until| (until - now).to_std().ok())
            .unwrap_or(Duration::ZERO)
    }

    /// Forgets turned-away connections well past their suggested retry time.
    fn prune(&mut self, now: DateTime<Local>) {
        let grace = chrono::Duration::from_std(EXPECTED_RETURN_GRACE).unwrap_or_default();
        self.expected_returns.retain(|retry_at| *retry_at + grace > now);
    }

    /// Turns a connection away: it should retry once a card should be free — the later
    /// of the queue estimate and the existing backlog, and no sooner than
    /// `min_retry`. Its own session then extends the backlog.
    fn turn_away(&mut self, snapshot: &QueueSnapshot, now: DateTime<Local>, min_retry: Duration) -> Duration {
        self.prune(now);
        let retry_after = match snapshot.estimated_wait() {
            Some(wait) => wait.max(self.remaining(now)).max(min_retry),
            None => NO_CARDS_RETRY.max(min_retry),
        };
        let retry_at = now + chrono::Duration::from_std(retry_after).unwrap_or_default();
        let share = chrono::Duration::from_std(snapshot.backlog_share()).unwrap_or_default();
        self.busy_until = Some(retry_at + share);
        self.expected_returns.push_back(retry_at);
        retry_after
    }
}

/// Card session history and backlog for one company, or for all of them together.
#[derive(Default)]
struct Stats {
    /// Most recent card session lengths, oldest first.
    sessions: VecDeque<Duration>,
    backlog: Backlog,
}

impl Stats {
    fn with_history(sessions: Vec<Duration>) -> Self {
        Self { sessions: sessions.into(), backlog: Backlog::default() }
    }

    fn record(&mut self, duration: Duration) {
        if self.sessions.len() == SESSION_SAMPLES {
            self.sessions.pop_front();
        }
        self.sessions.push_back(duration);
    }

    fn average_session(&self) -> Option<Duration> {
        if self.sessions.is_empty() {
            return None;
        }
        Some(self.sessions.iter().sum::<Duration>() / self.sessions.len() as u32)
    }
}

#[derive(Default)]
struct State {
    /// Every company together.
    all: Stats,
    companies: HashMap<String, Stats>,
}

impl State {
    /// `company`'s average session, falling back to the all-company average and then
    /// `DEFAULT_CARD_SESSION` until there's history. `None` for the all-company figure.
    fn average_session(&self, company: Option<&str>) -> Duration {
        company
            .and_then(|company| self.companies.get(company))
            .and_then(Stats::average_session)
            .or_else(|| self.all.average_session())
            .unwrap_or(DEFAULT_CARD_SESSION)
    }

    fn stats(&mut self, company: Option<&str>) -> &mut Stats {
        match company {
            Some(company) => self.companies.entry(company.to_string()).or_default(),
            None => &mut self.all,
        }
    }
}

lazy_static! {
    static ref STATE: Mutex<State> = Mutex::new(State::default());
}

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Loads recent card session lengths from the database, for every configured company
/// and overall. Blocking; call once at startup, off the async runtime.
pub fn load_history() {
    let all = crate::database::recent_card_sessions_blocking(None, SESSION_SAMPLES);
    let companies: HashMap<String, Stats> = crate::config::company_ids()
        .into_iter()
        .map(|company| {
            let history = crate::database::recent_card_sessions_blocking(Some(&company), SESSION_SAMPLES);
            (company, Stats::with_history(history))
        })
        .collect();
    let mut state = state();
    state.all = Stats::with_history(all);
    state.companies = companies;
    log::info!(
        "[ADMISSION] status=history_loaded sessions={} average_session_ms={}",
        state.all.sessions.len(),
        state.average_session(None).as_millis()
    );
}

/// Records how long one of `company`'s devices held a card. Called from `CardGuard`'s
/// drop.
pub fn record_card_session(company: &str, duration: Duration) {
    {
        let mut state = state();
        state.all.record(duration);
        state.stats(Some(company)).record(duration);
    }
    log::debug!("[ADMISSION] company={} card_session_ms={}", company, duration.as_millis());
    #[cfg(not(test))]
    {
        let company = company.to_string();
        crate::rt::spawn_blocking(move || {
            crate::database::record_card_session_blocking(company, duration)
        });
    }
}

/// The queue for one company (its own pool), or every company's pools added together
/// for `None` — used before a device has announced and its company is known.
pub fn snapshot(company: Option<&str>) -> QueueSnapshot {
    let pools = match company {
        Some(company) => vec![crate::card_pool::CARD_POOLS.pool(company)],
        None => crate::card_pool::CARD_POOLS.all(),
    };
    QueueSnapshot {
        cards: pools.iter().map(|pool| pool.card_count()).sum(),
        idle_cards: pools.iter().map(|pool| pool.idle_count()).sum(),
        waiting: pools.iter().map(|pool| pool.waiting_count()).sum(),
        average_session: state().average_session(company),
    }
}

/// How busy the server is: the live queue plus connections we've turned away and
/// expect back.
#[derive(Clone, Copy, Debug)]
pub struct Busyness {
    pub queue: QueueSnapshot,
    /// Turned-away connections we expect back.
    pub expected_returns: usize,
    /// Rough guide to when a card will next be free for a new arrival: the later of the
    /// queue estimate and the backlog timer. `None` if there are no cards.
    pub card_available_in: Option<Duration>,
}

/// Busyness of one company, or of everything together for `None`.
pub fn busyness(company: Option<&str>) -> Busyness {
    let queue = snapshot(company);
    let now = Local::now();
    let mut state = state();
    let stats = state.stats(company);
    stats.backlog.prune(now);
    let backlog = stats.backlog.remaining(now);
    Busyness {
        queue,
        expected_returns: stats.backlog.expected_returns.len(),
        card_available_in: queue.estimated_wait().map(|wait| wait.max(backlog)),
    }
}

/// A connection is being turned away over the global `connection_limit`, before it has
/// announced: how long it should wait before retrying (at least `min_retry`), from all
/// companies' queues together. Records it in the all-company backlog.
pub fn over_connection_limit_retry_delay(min_retry: Duration) -> Duration {
    let snapshot = snapshot(None);
    state().all.backlog.turn_away(&snapshot, Local::now(), min_retry)
}

/// One of `company`'s devices is being turned away over the company's connection limit:
/// how long it should wait before retrying (at least `min_retry`), from that company's
/// queue. Records it in the company's backlog.
pub fn over_company_limit_retry_delay(company: &str, min_retry: Duration) -> Duration {
    let snapshot = snapshot(Some(company));
    state().stats(Some(company)).backlog.turn_away(&snapshot, Local::now(), min_retry)
}

/// If the device may not download yet (`download_allowed_at` from
/// `database::next_download_allowed_at`), how long it should wait before retrying:
/// until then, plus a random spread so devices don't all return at midnight together.
pub fn download_interval_retry_delay(download_allowed_at: Option<DateTime<Local>>) -> Option<Duration> {
    let spread = Duration::from_secs(rand::thread_rng().gen_range(0..=MIDNIGHT_SPREAD_SECONDS));
    download_interval_delay(download_allowed_at, Local::now(), spread)
}

fn download_interval_delay(
    download_allowed_at: Option<DateTime<Local>>,
    now: DateTime<Local>,
    spread: Duration,
) -> Option<Duration> {
    let until_allowed = (download_allowed_at? - now).to_std().ok()?;
    Some(until_allowed + spread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const MIN_RETRY: Duration = Duration::from_secs(5);

    fn snapshot(cards: usize, idle_cards: usize, waiting: usize, average_secs: u64) -> QueueSnapshot {
        QueueSnapshot { cards, idle_cards, waiting, average_session: Duration::from_secs(average_secs) }
    }

    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 28, h, m, s).earliest().unwrap()
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn company_average_falls_back_to_overall_then_default() {
        let mut state = State::default();
        assert_eq!(state.average_session(Some("acme")), DEFAULT_CARD_SESSION);
        state.all.record(secs(60));
        assert_eq!(state.average_session(Some("acme")), secs(60));
        state.stats(Some("acme")).record(secs(20));
        assert_eq!(state.average_session(Some("acme")), secs(20));
        assert_eq!(state.average_session(None), secs(60));
    }

    #[test]
    fn estimated_wait() {
        assert_eq!(snapshot(0, 0, 0, 120).estimated_wait(), None);
        assert_eq!(snapshot(2, 1, 0, 120).estimated_wait(), Some(Duration::ZERO));
        // 2 cards busy, 3 waiting: (3 + 1) × 120 s ÷ 2 = 240 s.
        assert_eq!(snapshot(2, 0, 3, 120).estimated_wait(), Some(secs(240)));
    }

    #[test]
    fn each_turned_away_connection_is_told_a_later_time() {
        let mut backlog = Backlog::default();
        let now = at(12, 0, 0);
        // 2 cards, 3 waiting, 120 s sessions: queue estimate 240 s.
        let snap = snapshot(2, 0, 3, 120);
        assert_eq!(backlog.turn_away(&snap, now, MIN_RETRY), secs(240));
        // The first one's share (80% of 120 ÷ 2 = 48 s) pushes the next one later.
        assert_eq!(backlog.turn_away(&snap, now, MIN_RETRY), secs(288));
        assert_eq!(backlog.expected_returns.len(), 2);
    }

    #[test]
    fn backlog_runs_down_in_real_time() {
        let mut backlog = Backlog::default();
        backlog.turn_away(&snapshot(2, 0, 3, 120), at(12, 0, 0), MIN_RETRY);
        assert_eq!(backlog.remaining(at(12, 0, 0)), secs(288));
        assert_eq!(backlog.remaining(at(12, 4, 0)), secs(48));
        assert_eq!(backlog.remaining(at(12, 10, 0)), Duration::ZERO);
    }

    #[test]
    fn retry_is_never_sooner_than_the_minimum() {
        let mut backlog = Backlog::default();
        // A card is free, so the queue estimate is zero — still wait MIN_RETRY.
        assert_eq!(backlog.turn_away(&snapshot(2, 1, 0, 120), at(12, 0, 0), MIN_RETRY), MIN_RETRY);
    }

    #[test]
    fn no_cards_uses_the_fallback_delay() {
        let mut backlog = Backlog::default();
        assert_eq!(backlog.turn_away(&snapshot(0, 0, 0, 120), at(12, 0, 0), MIN_RETRY), NO_CARDS_RETRY);
    }

    #[test]
    fn old_expected_returns_are_forgotten() {
        let mut backlog = Backlog::default();
        backlog.turn_away(&snapshot(2, 0, 3, 120), at(12, 0, 0), MIN_RETRY);
        backlog.prune(at(14, 0, 0));
        assert!(backlog.expected_returns.is_empty());
    }

    #[test]
    fn download_interval_delay_is_time_until_allowed_plus_spread() {
        let now = at(22, 0, 0);
        let midnight = now + chrono::Duration::hours(2);
        assert_eq!(download_interval_delay(Some(midnight), now, secs(90)), Some(secs(2 * 3600 + 90)));
        assert_eq!(download_interval_delay(None, now, secs(90)), None);
        // Already allowed.
        assert_eq!(download_interval_delay(Some(now - chrono::Duration::hours(1)), now, secs(90)), None);
    }
}
