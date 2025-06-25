//! Persistence repositories for wallet synchronization.
//!
//! This module defines repository traits and file-based implementations for persisting wallet state,
//! ledger state, and transaction checkpoints. These repositories abstract the details of reading and
//! writing state to disk, enabling robust and resumable synchronization.
//!
//! The repositories are used by the state persistence service to save and restore wallet and ledger
//! state, as well as to manage transaction checkpoints for fast recovery.
//!
//! - `WalletStateRepository`: Interface for saving/loading wallet state.
//! - `LedgerStateRepository`: Interface for saving/loading ledger state.
//! - `CheckpointRepository`: Interface for saving/loading and cleaning up transaction checkpoints.
//! - `File*Repository`: File-based implementations of the above traits.

use crate::wallet::WalletSyncError;

use midnight_node_ledger_helpers::{DefaultDB, LedgerState, Serializable, WalletSeed, WalletState};
use midnight_serialize::Deserializable;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Repository for wallet state persistence.
///
/// This trait defines the interface for saving and loading wallet state to persistent storage.
#[async_trait::async_trait]
pub trait WalletStateRepository {
	/// Save the wallet state for a given seed and height.
	async fn save(
		&self,
		seed: &WalletSeed,
		state: &WalletState<DefaultDB>,
		height: u64,
	) -> Result<(), WalletSyncError>;
	/// Load the wallet state for a given seed, returning the state and height if found.
	async fn load(
		&self,
		seed: &WalletSeed,
	) -> Result<Option<(WalletState<DefaultDB>, u64)>, WalletSyncError>;
}

/// Repository for ledger state persistence.
///
/// This trait defines the interface for saving and loading ledger state to persistent storage.
#[async_trait::async_trait]
pub trait LedgerStateRepository {
	/// Save the ledger state at a given height.
	async fn save(
		&self,
		state: &LedgerState<DefaultDB>,
		height: u64,
	) -> Result<(), WalletSyncError>;
	/// Load the latest ledger state and height if found.
	async fn load(&self) -> Result<Option<(LedgerState<DefaultDB>, u64)>, WalletSyncError>;
}

/// Repository for checkpoint management.
///
/// This trait defines the interface for saving, loading, and cleaning up transaction checkpoints.
#[async_trait::async_trait]
pub trait CheckpointRepository {
	/// Save a checkpoint of transactions at a given height.
	async fn save(&self, transactions: &[String], height: u64) -> Result<(), WalletSyncError>;
	/// Find the latest checkpoint file and its height.
	async fn find_latest(&self) -> Result<Option<(PathBuf, u64)>, WalletSyncError>;
	/// Load transactions and height from a checkpoint file.
	async fn load(&self, path: &Path) -> Result<(Vec<String>, u64), WalletSyncError>;
	/// Clean up old checkpoint files, keeping only the most recent N.
	async fn cleanup_old(&self, keep_count: usize) -> Result<(), WalletSyncError>;
}

/// File-based implementation of WalletStateRepository.
///
/// Persists wallet state and metadata to disk in the specified data directory.
pub struct FileWalletStateRepository {
	data_dir: PathBuf,
}

impl FileWalletStateRepository {
	/// Create a new file-based wallet state repository for the given data directory.
	pub fn new(data_dir: PathBuf) -> Self {
		Self { data_dir }
	}

	/// Get the filename for the wallet state file for a given seed.
	fn get_wallet_filename(&self, seed: &WalletSeed) -> PathBuf {
		self.data_dir
			.join(format!("wallet_state_{}.bin", hex::encode(seed.0)))
	}

	/// Get the filename for the wallet state metadata file for a given seed.
	fn get_metadata_filename(&self, seed: &WalletSeed) -> PathBuf {
		self.data_dir
			.join(format!("wallet_state_{}.meta.json", hex::encode(seed.0)))
	}
}

#[async_trait::async_trait]
impl WalletStateRepository for FileWalletStateRepository {
	/// Save the wallet state and metadata to disk.
	async fn save(
		&self,
		seed: &WalletSeed,
		state: &WalletState<DefaultDB>,
		height: u64,
	) -> Result<(), WalletSyncError> {
		// Create metadata
		let metadata = serde_json::json!({
			"sync_height": height,
			"timestamp": chrono::Utc::now().to_rfc3339(),
		});

		// Save metadata
		let metadata_filename = self.get_metadata_filename(seed);
		tokio::fs::write(
			&metadata_filename,
			serde_json::to_string_pretty(&metadata).unwrap(),
		)
		.await
		.map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to write wallet state metadata: {}", e))
		})?;

		// Serialize wallet state
		let mut state_bytes = Vec::new();
		Serializable::serialize(state, &mut state_bytes).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to serialize wallet state: {}", e))
		})?;

		// Write state file
		let filename = self.get_wallet_filename(seed);
		tokio::fs::write(&filename, &state_bytes)
			.await
			.map_err(|e| {
				WalletSyncError::ParseError(format!("Failed to write wallet state file: {}", e))
			})?;

		info!("Saved wallet state to {:?} at height {}", filename, height);
		Ok(())
	}

	/// Load the wallet state and metadata from disk.
	async fn load(
		&self,
		seed: &WalletSeed,
	) -> Result<Option<(WalletState<DefaultDB>, u64)>, WalletSyncError> {
		let filename = self.get_wallet_filename(seed);
		let metadata_filename = self.get_metadata_filename(seed);

		// Check if files exist
		if !filename.exists() {
			return Ok(None);
		}

		// Load metadata
		let mut height = 0u64;
		if let Ok(meta_content) = tokio::fs::read_to_string(&metadata_filename).await {
			if let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&meta_content) {
				if let Some(h) = metadata.get("sync_height").and_then(|h| h.as_u64()) {
					height = h;
				}
			}
		}

		// Load state
		let state_bytes = tokio::fs::read(&filename).await.map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to read wallet state file: {}", e))
		})?;

		let mut cursor = Cursor::new(&state_bytes);
		let wallet_state: WalletState<DefaultDB> = Deserializable::deserialize(&mut cursor, 0)
			.map_err(|e| {
				WalletSyncError::ParseError(format!("Failed to deserialize wallet state: {}", e))
			})?;

		info!(
			"Loaded wallet state from {:?} at height {}",
			filename, height
		);
		Ok(Some((wallet_state, height)))
	}
}

/// File-based implementation of LedgerStateRepository.
///
/// Persists ledger state and metadata to disk in the specified data directory.
pub struct FileLedgerStateRepository {
	data_dir: PathBuf,
}

impl FileLedgerStateRepository {
	/// Create a new file-based ledger state repository for the given data directory.
	pub fn new(data_dir: PathBuf) -> Self {
		Self { data_dir }
	}
}

#[async_trait::async_trait]
impl LedgerStateRepository for FileLedgerStateRepository {
	/// Save the ledger state and metadata to disk.
	async fn save(
		&self,
		state: &LedgerState<DefaultDB>,
		height: u64,
	) -> Result<(), WalletSyncError> {
		// Create metadata
		let metadata = serde_json::json!({
			"sync_height": height,
			"timestamp": chrono::Utc::now().to_rfc3339(),
		});

		// Save metadata
		let metadata_filename = self.data_dir.join("ledger_state.meta.json");
		tokio::fs::write(
			&metadata_filename,
			serde_json::to_string_pretty(&metadata).unwrap(),
		)
		.await
		.map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to write ledger state metadata: {}", e))
		})?;

		// Serialize ledger state
		let mut state_bytes = Vec::new();
		Serializable::serialize(state, &mut state_bytes).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to serialize ledger state: {}", e))
		})?;

		// Write state file
		let filename = self.data_dir.join("ledger_state.bin");
		tokio::fs::write(&filename, &state_bytes)
			.await
			.map_err(|e| {
				WalletSyncError::ParseError(format!("Failed to write ledger state file: {}", e))
			})?;

		info!("Saved ledger state to {:?} at height {}", filename, height);
		Ok(())
	}

	/// Load the ledger state and metadata from disk.
	async fn load(&self) -> Result<Option<(LedgerState<DefaultDB>, u64)>, WalletSyncError> {
		let filename = self.data_dir.join("ledger_state.bin");
		let metadata_filename = self.data_dir.join("ledger_state.meta.json");

		// Check if files exist
		if !filename.exists() {
			return Ok(None);
		}

		// Load metadata
		let mut height = 0u64;
		if let Ok(meta_content) = tokio::fs::read_to_string(&metadata_filename).await {
			if let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&meta_content) {
				if let Some(h) = metadata.get("sync_height").and_then(|h| h.as_u64()) {
					height = h;
				}
			}
		}

		// Load state
		let state_bytes = tokio::fs::read(&filename).await.map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to read ledger state file: {}", e))
		})?;

		let mut cursor = Cursor::new(&state_bytes);
		let ledger_state: LedgerState<DefaultDB> = Deserializable::deserialize(&mut cursor, 0)
			.map_err(|e| {
				WalletSyncError::ParseError(format!("Failed to deserialize ledger state: {}", e))
			})?;

		info!(
			"Loaded ledger state from {:?} at height {}",
			filename, height
		);
		Ok(Some((ledger_state, height)))
	}
}

/// File-based implementation of CheckpointRepository.
///
/// Persists transaction checkpoints to disk in the specified data directory.
pub struct FileCheckpointRepository {
	data_dir: PathBuf,
}

impl FileCheckpointRepository {
	/// Create a new file-based checkpoint repository for the given data directory.
	pub fn new(data_dir: PathBuf) -> Self {
		Self { data_dir }
	}

	/// Get the filename for a checkpoint file at a given height.
	fn get_checkpoint_filename(&self, height: u64) -> PathBuf {
		self.data_dir
			.join(format!("checkpoint_transactions_height_{}.json", height))
	}
}

#[async_trait::async_trait]
impl CheckpointRepository for FileCheckpointRepository {
	/// Save a checkpoint of transactions to disk.
	async fn save(&self, transactions: &[String], height: u64) -> Result<(), WalletSyncError> {
		let checkpoint_file_path = self.get_checkpoint_filename(height);

		let content = serde_json::to_string_pretty(transactions).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to serialize checkpoint: {}", e))
		})?;

		tokio::fs::write(&checkpoint_file_path, content)
			.await
			.map_err(|e| {
				WalletSyncError::ParseError(format!("Failed to write checkpoint file: {}", e))
			})?;

		info!(
			"Checkpoint saved: {} transactions written to {:?}",
			transactions.len(),
			checkpoint_file_path
		);
		Ok(())
	}

	/// Find the latest checkpoint file and its height.
	async fn find_latest(&self) -> Result<Option<(PathBuf, u64)>, WalletSyncError> {
		let mut entries = tokio::fs::read_dir(&self.data_dir)
			.await
			.map_err(|e| WalletSyncError::ParseError(format!("Failed to read directory: {}", e)))?;

		let mut latest: Option<(PathBuf, u64)> = None;

		while let Some(entry) = entries.next_entry().await.map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to read directory entry: {}", e))
		})? {
			let path = entry.path();
			if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
				if filename.starts_with("checkpoint_transactions_height_")
					&& filename.ends_with(".json")
				{
					if let Some(height_str) = filename
						.strip_prefix("checkpoint_transactions_height_")
						.and_then(|s| s.strip_suffix(".json"))
					{
						if let Ok(height) = height_str.parse::<u64>() {
							if latest.as_ref().map(|(_, h)| height > *h).unwrap_or(true) {
								latest = Some((path, height));
							}
						}
					}
				}
			}
		}

		Ok(latest)
	}

	/// Load transactions and height from a checkpoint file.
	async fn load(&self, path: &Path) -> Result<(Vec<String>, u64), WalletSyncError> {
		let content = tokio::fs::read_to_string(path).await.map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to read checkpoint file: {}", e))
		})?;

		let transactions: Vec<String> = serde_json::from_str(&content).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to parse checkpoint file: {}", e))
		})?;

		// Extract height from filename
		let height = path
			.file_name()
			.and_then(|f| f.to_str())
			.and_then(|f| f.strip_prefix("checkpoint_transactions_height_"))
			.and_then(|s| s.strip_suffix(".json"))
			.and_then(|s| s.parse::<u64>().ok())
			.ok_or_else(|| {
				WalletSyncError::ParseError("Invalid checkpoint filename format".to_string())
			})?;

		info!(
			"Loaded {} transactions from checkpoint at height {}",
			transactions.len(),
			height
		);
		Ok((transactions, height))
	}

	/// Clean up old checkpoint files, keeping only the most recent N.
	async fn cleanup_old(&self, keep_count: usize) -> Result<(), WalletSyncError> {
		let mut entries = tokio::fs::read_dir(&self.data_dir)
			.await
			.map_err(|e| WalletSyncError::ParseError(format!("Failed to read directory: {}", e)))?;

		let mut checkpoints: Vec<(PathBuf, u64)> = Vec::new();

		while let Some(entry) = entries.next_entry().await.map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to read directory entry: {}", e))
		})? {
			let path = entry.path();
			if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
				if filename.starts_with("checkpoint_transactions_height_")
					&& filename.ends_with(".json")
				{
					if let Some(height_str) = filename
						.strip_prefix("checkpoint_transactions_height_")
						.and_then(|s| s.strip_suffix(".json"))
					{
						if let Ok(height) = height_str.parse::<u64>() {
							checkpoints.push((path, height));
						}
					}
				}
			}
		}

		if checkpoints.len() <= keep_count {
			return Ok(());
		}

		// Sort by height in descending order
		checkpoints.sort_by_key(|(_, height)| std::cmp::Reverse(*height));

		// Remove old checkpoints
		for (path, _) in checkpoints.into_iter().skip(keep_count) {
			if let Err(e) = tokio::fs::remove_file(&path).await {
				warn!("Failed to remove old checkpoint {:?}: {}", path, e);
			} else {
				info!("Removed old checkpoint: {:?}", path);
			}
		}

		Ok(())
	}
}
