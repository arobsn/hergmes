use crate::clients::node::NodeError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    NodeError(#[from] NodeError),
}
