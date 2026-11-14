use std::{collections::HashMap, sync::Arc, time::Duration};

use dotenvy::dotenv;
use hergmes::{
    adapters::{Follower, TokenChecker, loader::Loader},
    clients::NodeClient,
    config::Config,
    engine::Engine,
    error::AppError,
    runtime::{self, Keeper},
    trace::{self, default_subscriber},
};
use tokio::task::JoinSet;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), AppError> {
    if let Err(e) = dotenv() {
        warn!("Failed to load .env file: {:?}. Using environment variables instead.", e);
    }
    trace::init(default_subscriber());

    let result = run().await;
    if let Err(error) = &result {
        error!(%error, "Stopping");
    }
    result
}

/// Starts the engine task, the follower and a keeper per configured key, and runs until the
/// engine is gone. The engine task ends only by panicking, which can leave it half-changed,
/// and the follower and keepers then stop with `EngineGone`: nothing can serve, so the
/// process exits, and a restart reloads every key from the node. A keeper that gives up its
/// key (logged), or panics, leaves the relay running: one key's trouble never stops it.
async fn run() -> Result<(), AppError> {
    let config = Config::from_env()?;
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client");
    let node = NodeClient::connect(http_client, &config.node_url).await?;
    let keys = config.tracked_keys(node.network())?;

    let (engine, mut engine_task) = runtime::spawn(Engine::new(config.engine));
    info!(
        network = ?node.network(),
        window = config.engine.window,
        grace = config.engine.grace,
        tracked = keys.len(),
        "Following the node"
    );

    let follower = Follower::new(node.clone(), config.follower);
    let mut follower = tokio::spawn({
        let engine = engine.clone();
        async move { follower.run(&engine).await }
    });

    let keeper = Arc::new(Keeper::new(
        engine,
        Loader::new(node.clone(), Default::default()),
        Arc::new(TokenChecker::new(node, config.token_supply_limit)),
        Default::default(),
    ));
    let mut keepers = JoinSet::new();
    let mut kept = HashMap::new();
    for key in keys {
        let keeper = keeper.clone();
        let task = keepers.spawn({
            let key = key.clone();
            async move { keeper.keep(key).await }
        });
        kept.insert(task.id(), key);
    }
    drop(keeper);

    let gone = loop {
        tokio::select! {
            biased;
            ended = &mut engine_task => {
                ended?;
                // Every handle was dropped, so the follower has ended: report why.
                return Err(follower.await?.into());
            }
            gone = &mut follower => break gone?,
            Some(stopped) = keepers.join_next_with_id() => match stopped {
                Ok((id, Ok(()))) => {
                    kept.remove(&id);
                }
                Ok((_, Err(gone))) => break gone,
                Err(error) => {
                    let key = kept.remove(&error.id());
                    error!(?key, %error, "Keeper of a configured key failed, giving the key up");
                }
            },
        }
    };
    // `EngineGone` means the engine task is ending, but a task that saw it can finish first:
    // the inbox and the pending replies are dropped while the panic unwinds, before the join
    // handle is ready. The panic is the cause worth reporting, so it is waited for.
    engine_task.await?;
    Err(gone.into())
}
