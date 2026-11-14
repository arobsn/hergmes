use crate::{adapters::EngineGone, clients::node::NodeError, config::ConfigError};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    NodeError(#[from] NodeError),

    #[error(transparent)]
    EngineGone(#[from] EngineGone),

    #[error("Background task failed: {0}")]
    TaskFailed(#[from] tokio::task::JoinError),
}
