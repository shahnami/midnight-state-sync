//! Merkle tree update service for wallet synchronization.
//!
//! This module provides the `MerkleTreeUpdateService`, which is responsible for parsing and applying
//! Merkle tree updates to the wallet state. It ensures that the wallet's view of the blockchain's
//! Merkle tree is kept up to date, which is essential for transaction validation and balance calculation.
//!
//! The service is used by the orchestrator and event handlers to process Merkle update events and
//! apply them in the correct order during synchronization.

use crate::indexer::CollapsedUpdateInfo;
use crate::wallet::WalletSyncError;

use midnight_ledger_prototype::transient_crypto::merkle_tree::MerkleTreeCollapsedUpdate;
use midnight_node_ledger_helpers::{
    DefaultDB, LedgerContext, NetworkId, WalletSeed, mn_ledger_serialize::deserialize,
};
use std::sync::Arc;
use tracing::{debug, error};

#[derive(Clone)]
/// Service for parsing and applying Merkle tree updates to wallet state.
///
/// This service is responsible for decoding, deserializing, and applying Merkle tree updates
/// received during synchronization. It interacts with the wallet context to update the wallet's
/// internal state.
pub struct MerkleTreeUpdateService {
    context: Arc<LedgerContext<DefaultDB>>,
    network: NetworkId,
}

impl MerkleTreeUpdateService {
    /// Create a new MerkleTreeUpdateService for the given context and network.
    pub fn new(context: Arc<LedgerContext<DefaultDB>>, network: NetworkId) -> Self {
        Self { context, network }
    }

    /// Parse raw collapsed update hex into a MerkleTreeCollapsedUpdate type.
    ///
    /// This method decodes and deserializes the update data for further processing.
    pub async fn parse_collapsed_update(
        &self,
        update_info: &CollapsedUpdateInfo,
    ) -> Result<MerkleTreeCollapsedUpdate, WalletSyncError> {
        let update_bytes = hex::decode(&update_info.update_data).map_err(|e| {
            error!("Failed to decode hex: {}", e);
            WalletSyncError::MerkleTreeUpdateError(format!("Failed to decode hex: {}", e))
        })?;

        let collapsed_update: MerkleTreeCollapsedUpdate =
            deserialize(&update_bytes[..], self.network).map_err(|e| {
                error!("Failed to deserialize collapsed update: {}", e);
                WalletSyncError::MerkleTreeUpdateError(format!(
                    "Failed to deserialize collapsed update: {}",
                    e
                ))
            })?;

        Ok(collapsed_update)
    }

    /// Apply a collapsed update to a specific wallet.
    ///
    /// This method updates the wallet's state with the provided Merkle tree update.
    pub fn apply_collapsed_update(
        &self,
        wallet_seed: &WalletSeed,
        collapsed_update: &MerkleTreeCollapsedUpdate,
    ) -> Result<(), WalletSyncError> {
        let mut wallets_guard = self.context.wallets.lock().unwrap();

        let wallet = wallets_guard.get_mut(wallet_seed).ok_or_else(|| {
            WalletSyncError::MerkleTreeUpdateError(format!(
                "Wallet with seed {:?} not found in context",
                wallet_seed
            ))
        })?;

        match wallet.state.apply_collapsed_update(collapsed_update) {
            Ok(new_state) => {
                debug!(
                    "Applied collapsed update: start={}, end={}, new first_free={} (was {})",
                    collapsed_update.start,
                    collapsed_update.end,
                    new_state.first_free,
                    wallet.state.first_free
                );
                wallet.update_state(new_state);
                Ok(())
            }
            Err(e) => {
                error!("Failed to apply collapsed update: {}", e);
                Err(WalletSyncError::MerkleTreeUpdateError(format!(
                    "Failed to apply collapsed update to wallet state: {}",
                    e
                )))
            }
        }
    }

    /// Process a collapsed update info and return the parsed update.
    ///
    /// This method logs the update and parses it for later application.
    pub async fn process_collapsed_update(
        &self,
        update_info: &CollapsedUpdateInfo,
    ) -> Result<MerkleTreeCollapsedUpdate, WalletSyncError> {
        debug!(
            "Processing collapsed update at blockchain index {} - start: {}, end: {}",
            update_info.blockchain_index, update_info.start, update_info.end
        );

        // Log current wallet state for debugging
        {
            let wallets_guard = self.context.wallets.lock().unwrap();
            if let Some(wallet) = wallets_guard.get(&WalletSeed::from(
                "151f521307dab9fd5339aabc81542d30e2c33b0e0a1de5e80f3b343ccd72b7d4",
            )) {
                debug!(
                    "Current wallet state before update: first_free={}",
                    wallet.state.first_free
                );
            }
        }

        self.parse_collapsed_update(update_info).await
    }
}
