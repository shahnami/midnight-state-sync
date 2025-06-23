use crate::indexer::IndexerError;

use serde::{Deserialize, Serialize};

/// Information about a collapsed update from the indexer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapsedUpdateInfo {
	pub blockchain_index: u64,
	pub protocol_version: u32,
	pub start: u64,
	pub end: u64,
	pub update_data: String,
}

/// Enhanced error types for session-based wallet sync
#[allow(clippy::enum_variant_names)]
#[derive(Debug, thiserror::Error)]
pub enum WalletSyncError {
	#[error("Indexer error: {0}")]
	IndexerError(#[from] IndexerError),

	#[error("Session error: {0}")]
	SessionError(String),

	#[error("Viewing key error: {0}")]
	ViewingKeyError(String),

	#[error("Transaction parse error: {0}")]
	ParseError(String),

	#[error("IO error: {0}")]
	IoError(#[from] std::io::Error),

	#[error("Merkle tree update error: {0}")]
	MerkleTreeUpdateError(String),

	#[error("Sync error: {0}")]
	SyncError(String),
}
