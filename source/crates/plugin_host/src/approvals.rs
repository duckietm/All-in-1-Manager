//! The approval queue: nothing a plugin asks to run on the server (`run-script`) or to change in the
//! database (`sql-write`) happens before the user saw it in full and clicked Approve.
//!
//! One [`Approvals`] is shared (`Arc`) between the runtime, which queues requests with
//! [`Approvals::request`] after the permission checks, and the app, which shows [`Approvals::pending`]
//! and answers with [`Approvals::decide`]. A notifier wakes the UI whenever the pending list changes.
//!
//! A request leaves the queue when it is decided, when its waiter goes away (the guest call was
//! cancelled, for example because the page was closed), or with [`Approvals::reject_all_for`] (the
//! plugin's instance was dropped or poisoned). Waiting has no time limit: the user decides when.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::oneshot;

use crate::runtime::replaced;
use crate::services::WriteBatch;

/// The largest script a plugin may ask to run (bytes). The user reads all of it.
pub const MAX_SCRIPT: usize = 64 * 1024;

/// The user's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approved,
    Rejected,
}

/// What a plugin asks to do, as the user sees it before deciding.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    /// A script for the logged-in server (`plugin` is the plugin's manifest id).
    Script { plugin: String, script: String, as_root: bool },
    /// Database changes, applied in one transaction.
    Writes { plugin: String, batch: WriteBatch },
}

impl Pending {
    /// The id of the plugin asking.
    pub fn plugin(&self) -> &str {
        match self {
            Pending::Script { plugin, .. } | Pending::Writes { plugin, .. } => plugin,
        }
    }

    /// The text to show, ready as it is: the full script, or the summary lines of the changes (one per
    /// line). Lines are never cut; a character the log sanitiser would replace becomes U+FFFD (the
    /// runtime refuses scripts holding any, so a script is shown exactly as it runs).
    pub fn text(&self) -> String {
        match self {
            Pending::Script { script, .. } => shown(script),
            Pending::Writes { batch, .. } => batch.summary.iter().map(|l| shown(l)).collect::<Vec<_>>().join("\n"),
        }
    }
}

/// `text` with every character the sanitiser would replace, newlines excepted, as U+FFFD; uncut.
fn shown(text: &str) -> String {
    text.chars().map(|c| if c != '\n' && replaced(c) { '\u{FFFD}' } else { c }).collect()
}

/// Refuses a script that cannot be shown to the user exactly as it would run: empty, larger than
/// [`MAX_SCRIPT`], or holding a character the sanitiser would replace (control characters other than tab
/// and newline, carriage return included; bidi controls; invisible characters).
pub fn check_script(script: &str) -> Result<(), String> {
    if script.is_empty() {
        return Err("permission denied: an empty script".into());
    }
    if script.len() > MAX_SCRIPT {
        return Err(format!("permission denied: the script is larger than {} KiB", MAX_SCRIPT / 1024));
    }
    if script.chars().any(|c| c != '\n' && replaced(c)) {
        return Err("permission denied: the script holds control or invisible characters (only tab and newline \
                    are allowed), so it could not be shown exactly as it would run"
            .into());
    }
    Ok(())
}

type Notifier = Arc<dyn Fn() + Send + Sync>;

struct Entry {
    pending: Pending,
    answer: oneshot::Sender<Decision>,
}

#[derive(Default)]
struct State {
    next: u64,
    items: BTreeMap<u64, Entry>,
}

/// The queue of requests waiting for the user. `Send + Sync`; share it with `Arc`.
#[derive(Default)]
pub struct Approvals {
    state: Mutex<State>,
    notifier: Mutex<Option<Notifier>>,
}

impl Approvals {
    /// A queue without a notifier.
    pub fn new() -> Self {
        Self::default()
    }

    /// A queue that calls `notify` whenever the pending list changes (from any thread; never with the
    /// queue locked, so `notify` may call [`Approvals::pending`]).
    pub fn with_notifier(notify: impl Fn() + Send + Sync + 'static) -> Self {
        let a = Self::default();
        a.set_notifier(notify);
        a
    }

    /// Replaces the notifier.
    pub fn set_notifier(&self, notify: impl Fn() + Send + Sync + 'static) {
        *self.notifier.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(notify));
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify(&self) {
        let notifier = self.notifier.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(n) = notifier {
            n();
        }
    }

    /// Queues `p` (at once, before the future is first polled) and resolves with the user's decision.
    /// Dropping the future withdraws the request. A request that leaves the queue without a decision
    /// resolves as [`Decision::Rejected`].
    pub fn request(&self, p: Pending) -> impl Future<Output = Decision> + Send + '_ {
        let (answer, decision) = oneshot::channel();
        let id = {
            let mut s = self.lock();
            let id = s.next;
            s.next += 1;
            s.items.insert(id, Entry { pending: p, answer });
            id
        };
        self.notify();
        // Created here, not inside the async block, so it also runs if the future is never polled.
        let withdraw = Withdraw { approvals: self, id };
        async move {
            let _withdraw = withdraw;
            decision.await.unwrap_or(Decision::Rejected)
        }
    }

    /// The requests waiting for the user, oldest first.
    pub fn pending(&self) -> Vec<(u64, Pending)> {
        self.lock().items.iter().map(|(id, e)| (*id, e.pending.clone())).collect()
    }

    /// True while a request of `plugin` waits.
    pub fn has_pending_for(&self, plugin: &str) -> bool {
        self.lock().items.values().any(|e| e.pending.plugin() == plugin)
    }

    /// Answers request `id`. An unknown or already decided id does nothing.
    pub fn decide(&self, id: u64, d: Decision) {
        let entry = self.lock().items.remove(&id);
        if let Some(entry) = entry {
            let _ = entry.answer.send(d);
            self.notify();
        }
    }

    /// Rejects every request of `plugin` (its page was closed or its instance failed); other plugins'
    /// requests stay.
    pub fn reject_all_for(&self, plugin: &str) {
        let gone: Vec<Entry> = {
            let mut s = self.lock();
            let ids: Vec<u64> = s.items.iter().filter(|(_, e)| e.pending.plugin() == plugin).map(|(id, _)| *id).collect();
            ids.into_iter().filter_map(|id| s.items.remove(&id)).collect()
        };
        if gone.is_empty() {
            return;
        }
        for entry in gone {
            let _ = entry.answer.send(Decision::Rejected);
        }
        self.notify();
    }
}

/// Removes its request from the queue when the waiting future goes away undecided.
struct Withdraw<'a> {
    approvals: &'a Approvals,
    id: u64,
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        let removed = self.approvals.lock().items.remove(&self.id).is_some();
        if removed {
            self.approvals.notify();
        }
    }
}
