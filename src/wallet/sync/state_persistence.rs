//! State persistence service for wallet synchronization.
//!
//! This module provides the `StatePersistenceService`, which manages saving and restoring wallet and
//! ledger state, as well as transaction checkpoints. It uses repository traits to abstract the details
//! of file-based persistence, enabling robust and resumable synchronization.
//!
//! The service is used by the orchestrator and event handlers to persist state at key points during
//! sync, and to restore state on startup or recovery.

use crate::wallet::WalletSyncError;
use crate::wallet::sync::repositories::{
    CheckpointRepository, FileCheckpointRepository, FileLedgerStateRepository,
    FileWalletStateRepository, LedgerStateRepository, WalletStateRepository,
};

use midnight_node_ledger_helpers::{DefaultDB, LedgerContext, WalletSeed};
use std::path::PathBuf;
use tracing::info;

/// Service for managing state persistence operations.
///
/// This service coordinates the saving and loading of wallet state, ledger state, and transaction
/// checkpoints using the appropriate repository implementations. It is responsible for ensuring
/// that state is persisted and restored correctly for robust and resumable sync.
pub struct StatePersistenceService {
    wallet_repo: Box<dyn WalletStateRepository + Send + Sync>,
    ledger_repo: Box<dyn LedgerStateRepository + Send + Sync>,
    checkpoint_repo: Box<dyn CheckpointRepository + Send + Sync>,
}

impl StatePersistenceService {
    /// Create a new state persistence service for the given data directory.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            wallet_repo: Box::new(FileWalletStateRepository::new(data_dir.clone())),
            ledger_repo: Box::new(FileLedgerStateRepository::new(data_dir.clone())),
            checkpoint_repo: Box::new(FileCheckpointRepository::new(data_dir)),
        }
    }

    /// Save both wallet and ledger states at the given height.
    pub async fn save_states(
        &self,
        context: &LedgerContext<DefaultDB>,
        wallet_seed: &WalletSeed,
        height: u64,
    ) -> Result<(), WalletSyncError> {
        // Save wallet state
        let wallet = context.wallet_from_seed(*wallet_seed);
        self.wallet_repo
            .save(wallet_seed, &wallet.state, height)
            .await?;

        // Save ledger state
        let ledger_state = {
            let guard = context.ledger_state.lock().unwrap();
            guard.clone()
        };
        self.ledger_repo.save(&ledger_state, height).await?;

        Ok(())
    }

    /// Restore both wallet and ledger states, returning the restored height if available.
    pub async fn restore_states(
        &self,
        context: &LedgerContext<DefaultDB>,
        wallet_seed: &WalletSeed,
    ) -> Result<Option<u64>, WalletSyncError> {
        let wallet_result = self.wallet_repo.load(wallet_seed).await?;
        let ledger_result = self.ledger_repo.load().await?;

        let restored_height = match (wallet_result, ledger_result) {
            (Some((wallet_state, wallet_height)), Some((ledger_state, ledger_height))) => {
                // Use the minimum height to ensure consistency
                let min_height = wallet_height.min(ledger_height);

                // Update wallet state
                let mut wallets_guard = context.wallets.lock().unwrap();
                if let Some(wallet) = wallets_guard.get_mut(wallet_seed) {
                    wallet.update_state(wallet_state);
                    info!("Restored wallet state from height {}", wallet_height);
                }

                // Update ledger state
                let mut ledger_state_guard = context.ledger_state.lock().unwrap();
                *ledger_state_guard = ledger_state;
                info!("Restored ledger state from height {}", ledger_height);

                Some(min_height)
            }
            (Some((wallet_state, height)), None) => {
                // Only wallet state available
                let mut wallets_guard = context.wallets.lock().unwrap();
                if let Some(wallet) = wallets_guard.get_mut(wallet_seed) {
                    wallet.update_state(wallet_state);
                    info!("Restored wallet state from height {}", height);
                }
                Some(height)
            }
            (None, Some((ledger_state, height))) => {
                // Only ledger state available
                let mut ledger_state_guard = context.ledger_state.lock().unwrap();
                *ledger_state_guard = ledger_state;
                info!("Restored ledger state from height {}", height);
                Some(height)
            }
            (None, None) => None,
        };

        Ok(restored_height)
    }

    /// Save a checkpoint of raw transactions at the given height.
    pub async fn save_checkpoint(
        &self,
        transactions: &[String],
        height: u64,
    ) -> Result<(), WalletSyncError> {
        self.checkpoint_repo.save(transactions, height).await
    }

    /// Find and load the latest checkpoint, returning transactions and height if found.
    pub async fn load_latest_checkpoint(
        &self,
    ) -> Result<Option<(Vec<String>, u64)>, WalletSyncError> {
        if let Some((path, _)) = self.checkpoint_repo.find_latest().await? {
            let (transactions, height) = self.checkpoint_repo.load(&path).await?;
            Ok(Some((transactions, height)))
        } else {
            Ok(None)
        }
    }

    /// Clean up old checkpoints, keeping only the most recent N.
    pub async fn cleanup_checkpoints(&self, keep_count: usize) -> Result<(), WalletSyncError> {
        self.checkpoint_repo.cleanup_old(keep_count).await
    }
}

/// Configuration for checkpoint saving.
///
/// This struct controls how often checkpoints are saved and how many are retained.
#[derive(Clone)]
pub struct CheckpointConfig {
    /// Save checkpoint every N blocks.
    pub interval: u64,
    /// Number of checkpoints to keep.
    pub keep_count: usize,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            interval: 1000,
            keep_count: 2,
        }
    }
}
