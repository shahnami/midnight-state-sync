//! Types for GraphQL indexer integration with session management

use serde::{Deserialize, Serialize};

/// Transaction data from the indexer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionData {
	pub hash: String,
	pub identifiers: Option<Vec<String>>,
	pub raw: Option<String>,
	#[serde(rename = "applyStage")]
	pub apply_stage: Option<String>,
	#[serde(rename = "merkleTreeRoot")]
	pub merkle_tree_root: Option<String>,
	#[serde(rename = "protocolVersion")]
	pub protocol_version: Option<u32>,
}

/// Wallet sync event from wallet subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WalletSyncEvent {
	ViewingUpdate {
		#[serde(rename = "__typename")]
		type_name: String,
		index: u64,
		update: Vec<ZswapChainStateUpdate>,
	},
	ProgressUpdate {
		#[serde(rename = "__typename")]
		type_name: String,
		#[serde(rename = "highestIndex")]
		highest_index: u64,
		#[serde(rename = "highestRelevantIndex")]
		highest_relevant_index: u64,
		#[serde(rename = "highestRelevantWalletIndex")]
		highest_relevant_wallet_index: u64,
	},
}

/// Zswap chain state update from the indexer
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "__typename")]
pub enum ZswapChainStateUpdate {
	RelevantTransaction {
		transaction: TransactionData,
		#[serde(default)]
		start: u64,
		#[serde(default)]
		end: u64,
	},
	MerkleTreeCollapsedUpdate {
		#[serde(rename = "protocolVersion", default)]
		protocol_version: u32,
		#[serde(default)]
		start: u64,
		#[serde(default)]
		end: u64,
		#[serde(default)]
		update: String,
	},
}

/// Viewing key formats supported by the indexer
#[derive(Debug, Clone)]
pub enum ViewingKeyFormat {
	/// Bech32m format (preferred): mn_shield-esk_dev1...
	Bech32m(String),
}

impl ViewingKeyFormat {
	/// Get the viewing key as a string for API calls
	pub fn as_str(&self) -> &str {
		match self {
			ViewingKeyFormat::Bech32m(key) => key,
		}
	}
}

/// Enhanced error types for session management
#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
	#[error("GraphQL error: {0}")]
	GraphQLError(String),

	#[error("No data returned")]
	NoData,

	#[error("WebSocket error: {0}")]
	WebSocketError(#[from] tokio_tungstenite::tungstenite::Error),

	#[error("HTTP error: {0}")]
	HttpError(#[from] reqwest::Error),

	#[error("JSON parse error: {0}")]
	JsonError(#[from] serde_json::Error),

	#[error("Session error: {0}")]
	SessionError(String),
}
