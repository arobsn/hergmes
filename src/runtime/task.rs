//! The engine task: the one owner of the engine at runtime.
//!
//! It takes messages from one inbox, one at a time: events, queries, subscribes and
//! unsubscribes. Each event's outputs go to the subscribers of their keys before the caller
//! gets its reply, so every subscriber sees outputs in the order the engine produced them,
//! and a caller whose `apply` returned knows its outputs were delivered. The caller gets back
//! only the outputs addressed to it (`LoadRejected`). Waiting callers
//! park on a one-shot reply instead of blocking a worker thread on a lock (DESIGN.md,
//! "Decided", Runtime).
//!
//! The task also keeps the engine's subscriber counts. It sends `Subscribed` and
//! `Unsubscribed` itself, once per subscription it registers and ends, so the counts can't
//! drift from the registry: a subscription the engine ended with `resync_required` was
//! already dropped from the engine's count, and a late `Unsubscribed` for it would take one
//! off a key loaded again since.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tracing::{debug, info, warn};

use crate::{
    adapters::{EngineGone, EngineHandle},
    engine::{
        BlockRef, Engine, EngineError, Event, Output, Query, Reply, ResyncReason, TrackingKey,
    },
};

/// Messages the inbox holds before senders wait. Each one is a pending call, so this only
/// bounds how far callers can run ahead of the engine.
pub const INBOX: usize = 1024;

/// Where a subscription's live outputs go. Bounded: the task never waits on a subscriber,
/// so a full sink ends the subscription (see [`Handle::subscribe`]).
pub type Sink = mpsc::Sender<Arc<Output>>;

/// A registered subscription, unique for the life of the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionId(u64);

/// The answer to a subscribe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subscription {
    /// Subscribed. `catch_up` brings the client to the tip (replayed blocks or rollbacks,
    /// the overlay and the balance; only the balance for a balance-only subscription), and
    /// the live outputs follow in the sink.
    Live { id: SubscriptionId, catch_up: Vec<Output> },
    /// Not subscribed; nothing was registered. `not_tracked` means the key must be loaded
    /// first.
    Resync(ResyncReason),
}

enum Message {
    Apply(Event, oneshot::Sender<Result<Vec<Output>, EngineError>>),
    Query(Query, oneshot::Sender<Reply>),
    Subscribe {
        key: TrackingKey,
        from: Option<BlockRef>,
        sink: Sink,
        reply: oneshot::Sender<Subscription>,
    },
    Unsubscribe(SubscriptionId, oneshot::Sender<()>),
}

/// A handle to the engine task. Cheap to clone; the task runs until every handle is dropped.
#[derive(Debug, Clone)]
pub struct Handle {
    inbox: mpsc::Sender<Message>,
}

/// Starts the engine task on the current runtime.
///
/// The join handle ends with a panic if the engine panics. `main` watches it and exits: a
/// panic can leave the engine half-changed, so it is never restarted in place.
pub fn spawn(engine: Engine) -> (Handle, JoinHandle<()>) {
    let (inbox, messages) = mpsc::channel(INBOX);
    let task = EngineTask { engine, router: Router::default() };
    (Handle { inbox }, tokio::spawn(task.run(messages)))
}

impl Handle {
    /// Subscribes `sink` to `key`'s outputs, atomically with the catch-up from `from`: no
    /// event is applied between the two, so the live outputs start right after the
    /// catch-up. Without `from` the subscription is balance-only: its sink gets only
    /// `balance` and `resync_required`.
    ///
    /// The subscription ends, and the task drops the sink, when the engine sends the key a
    /// `resync_required` (delivered first), when the sink is full or closed, or on
    /// [`Handle::unsubscribe`]. A closed sink is noticed when an output is sent to it, or at
    /// the next block at the latest, so a dropped receiver never keeps a quiet key tracked.
    /// A receiver that sees the end of its stream without a `resync_required` was too slow,
    /// or dropped.
    pub async fn subscribe(
        &self,
        key: TrackingKey,
        from: Option<BlockRef>,
        sink: Sink,
    ) -> Result<Subscription, EngineGone> {
        self.call(|reply| Message::Subscribe { key, from, sink, reply })
            .await
    }

    /// Ends a subscription. When this returns, the sink gets nothing more. A subscription
    /// that already ended is ignored.
    pub async fn unsubscribe(&self, id: SubscriptionId) -> Result<(), EngineGone> {
        self.call(|reply| Message::Unsubscribe(id, reply)).await
    }

    async fn call<T>(
        &self,
        message: impl FnOnce(oneshot::Sender<T>) -> Message,
    ) -> Result<T, EngineGone> {
        let (reply, answer) = oneshot::channel();
        self.inbox
            .send(message(reply))
            .await
            .map_err(|_| EngineGone)?;
        answer.await.map_err(|_| EngineGone)
    }
}

impl EngineHandle for Handle {
    async fn apply(&self, event: Event) -> Result<Result<Vec<Output>, EngineError>, EngineGone> {
        self.call(|reply| Message::Apply(event, reply)).await
    }

    async fn query(&self, query: Query) -> Result<Reply, EngineGone> {
        self.call(|reply| Message::Query(query, reply)).await
    }
}

struct EngineTask {
    engine: Engine,
    router: Router,
}

impl EngineTask {
    async fn run(mut self, mut messages: mpsc::Receiver<Message>) {
        while let Some(message) = messages.recv().await {
            self.handle(message);
        }
    }

    /// A reply whose caller has gone is dropped: the work is done either way, except for a
    /// subscribe, which is undone.
    fn handle(&mut self, message: Message) {
        match message {
            Message::Apply(event, reply) => {
                if matches!(event, Event::BlockApplied(_)) {
                    // Idle keys are untracked on a block, so the subscribers gone since count
                    // off first.
                    self.router.sweep(&mut self.engine);
                }
                let result = self.engine.apply(event).map(|outputs| {
                    log(&outputs);
                    self.router.route(&mut self.engine, outputs)
                });
                if let Err(error) = &result {
                    warn!(%error, "Engine refused an event");
                }
                let _ = reply.send(result);
            }
            Message::Query(query, reply) => {
                let _ = reply.send(self.engine.query(query));
            }
            Message::Subscribe { key, from, sink, reply } => {
                let subscription = self
                    .router
                    .subscribe(&mut self.engine, key.clone(), from, sink);
                if let Subscription::Resync(reason) = &subscription {
                    info!(?key, ?reason, "Resync required");
                }
                if let Err(Subscription::Live { id, .. }) = reply.send(subscription) {
                    debug!(?id, "Subscriber left before its reply, unsubscribing");
                    self.router.unsubscribe(&mut self.engine, id);
                }
            }
            Message::Unsubscribe(id, reply) => {
                self.router.unsubscribe(&mut self.engine, id);
                let _ = reply.send(());
            }
        }
    }
}

struct Registered {
    key: TrackingKey,
    sink: Sink,
    balance_only: bool,
}

/// The live subscriptions, by id and by key.
#[derive(Default)]
struct Router {
    next: u64,
    subscriptions: BTreeMap<SubscriptionId, Registered>,
    by_key: HashMap<TrackingKey, BTreeSet<SubscriptionId>>,
}

impl Router {
    fn subscribe(
        &mut self,
        engine: &mut Engine,
        key: TrackingKey,
        from: Option<BlockRef>,
        sink: Sink,
    ) -> Subscription {
        let subscribed = engine
            .apply(Event::Subscribed(key.clone()))
            .expect("a subscribe is never refused");
        if let Some(reason) = resync_reason(&subscribed) {
            return Subscription::Resync(reason);
        }
        let catch_up = match engine.query(Query::CatchUp { key: key.clone(), from }) {
            Reply::CatchUp(outputs) => outputs,
            other => unreachable!("`Query::CatchUp` answered {other:?}"),
        };
        if let Some(reason) = resync_reason(&catch_up) {
            // The key is still tracked (the client's block is out of reach), so the count
            // `Subscribed` added comes off again.
            unsubscribe(engine, key);
            return Subscription::Resync(reason);
        }
        let id = SubscriptionId(self.next);
        self.next += 1;
        self.by_key.entry(key.clone()).or_default().insert(id);
        self.subscriptions
            .insert(id, Registered { key, sink, balance_only: from.is_none() });
        Subscription::Live { id, catch_up }
    }

    fn unsubscribe(&mut self, engine: &mut Engine, id: SubscriptionId) {
        if let Some(key) = self.end(id) {
            unsubscribe(engine, key);
        }
    }

    /// Drops a subscription from the registry, without telling the engine. Its key, if it was
    /// registered.
    fn end(&mut self, id: SubscriptionId) -> Option<TrackingKey> {
        let registered = self.subscriptions.remove(&id)?;
        if let Some(ids) = self.by_key.get_mut(&registered.key) {
            ids.remove(&id);
            if ids.is_empty() {
                self.by_key.remove(&registered.key);
            }
        }
        Some(registered.key)
    }

    /// Ends every subscription whose receiver was dropped. Routing notices a closed sink
    /// only when it sends to it, which a balance-only subscription to a quiet key never
    /// gets, and its count would keep the key tracked for good.
    fn sweep(&mut self, engine: &mut Engine) {
        let closed: Vec<SubscriptionId> = self
            .subscriptions
            .iter()
            .filter(|(_, registered)| registered.sink.is_closed())
            .map(|(id, _)| *id)
            .collect();
        for id in closed {
            debug!(?id, "Subscriber left, ending it");
            self.unsubscribe(engine, id);
        }
    }

    /// Sends each output to its key's subscribers, in order. Returns what goes to no one:
    /// `LoadRejected`, the loader's answer.
    fn route(&mut self, engine: &mut Engine, outputs: Vec<Output>) -> Vec<Output> {
        let mut unrouted = Vec::new();
        for output in outputs {
            if matches!(output, Output::LoadRejected { .. }) {
                unrouted.push(output);
                continue;
            }
            let Some(ids) = self.by_key.get(output.key()) else {
                continue;
            };
            let resync = matches!(output, Output::ResyncRequired { .. });
            let balance = matches!(output, Output::Balance { .. });
            // Moved into an `Arc` shared by the key's subscribers, only if one of them takes it.
            let mut output = Some(output);
            let mut shared: Option<Arc<Output>> = None;
            let mut ended = Vec::new();
            for id in ids {
                let registered = &self.subscriptions[id];
                if registered.balance_only && !balance && !resync {
                    continue;
                }
                let shared =
                    shared.get_or_insert_with(|| Arc::new(output.take().expect("moved only once")));
                if let Err(error) = registered.sink.try_send(shared.clone()) {
                    debug!(?id, key = ?registered.key, %error, "Subscriber can't take more, ending it");
                    ended.push(*id);
                }
            }
            if resync {
                // The engine untracked the key: every subscription to it ends, and none is
                // in the engine's count any more.
                ended.extend(ids.iter().copied());
            }
            for id in ended {
                if let Some(key) = self.end(id)
                    && !resync
                {
                    unsubscribe(engine, key);
                }
            }
        }
        unrouted
    }
}

fn unsubscribe(engine: &mut Engine, key: TrackingKey) {
    let outputs = engine
        .apply(Event::Unsubscribed(key))
        .expect("an unsubscribe is never refused");
    debug_assert!(outputs.is_empty(), "an unsubscribe has no outputs");
}

fn resync_reason(outputs: &[Output]) -> Option<ResyncReason> {
    outputs.iter().find_map(|o| match o {
        Output::ResyncRequired { reason, .. } => Some(*reason),
        _ => None,
    })
}

/// Logs what the engine returned to an event: the deltas of blocks and rollbacks that touch a
/// key, and every `resync_required`. The engine logs only what leaves no trace in its
/// outputs. A subscribe's `resync_required` is logged where it is answered.
fn log(outputs: &[Output]) {
    for output in outputs {
        match output {
            Output::Block { key, block, created, spent, .. }
                if !created.is_empty() || !spent.is_empty() =>
            {
                info!(
                    ?key,
                    height = block.height,
                    header_id = %block.header_id,
                    created = created.len(),
                    spent = spent.len(),
                    "Block delta"
                );
            }
            Output::Rollback { key, block, created, spent, .. }
                if !created.is_empty() || !spent.is_empty() =>
            {
                info!(
                    ?key,
                    height = block.height,
                    header_id = %block.header_id,
                    restored = created.len(),
                    removed = spent.len(),
                    "Rollback"
                );
            }
            Output::ResyncRequired { key, reason } => info!(?key, ?reason, "Resync required"),
            Output::LoadRejected { key, as_of } => debug!(?key, ?as_of, "Load rejected"),
            Output::Mempool { key, pending, .. } => {
                debug!(?key, txs = pending.len(), "Mempool overlay");
            }
            Output::Balance { key, balance } => debug!(?key, ?balance, "Balance"),
            Output::Block { .. } | Output::Rollback { .. } => {}
        }
    }
}

#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;
