use crate::clients::node::NodeError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Node error: {0}")]
    NodeError(#[from] NodeError),
}
