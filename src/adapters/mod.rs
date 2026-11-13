//! Adapters: all I/O against the node.
//!
//! Block consumer, mempool poller, first-load loader, box fetcher and balance
//! reader. They fetch, stamp with the observed tip and send events to the engine.

pub mod mempool;

pub use mempool::MempoolPoller;
