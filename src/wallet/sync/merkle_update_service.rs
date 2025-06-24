use crate::wallet::WalletSyncError;
use crate::wallet::types::CollapsedUpdateInfo;
use midnight_ledger_prototype::transient_crypto::merkle_tree::MerkleTreeCollapsedUpdate;
use midnight_node_ledger_helpers::{DefaultDB, LedgerContext, NetworkId, WalletSeed};
use midnight_serialize::deserialize;
use std::sync::Arc;
use tracing::{error, info};

#[derive(Clone)]
pub struct MerkleTreeUpdateService {
	context: Arc<LedgerContext<DefaultDB>>,
	network: NetworkId,
}

impl MerkleTreeUpdateService {
	pub fn new(context: Arc<LedgerContext<DefaultDB>>, network: NetworkId) -> Self {
		Self { context, network }
	}

	/// Parse raw collapsed update hex into MerkleTreeCollapsedUpdate type
	pub async fn parse_collapsed_update(
		&self,
		update_info: &CollapsedUpdateInfo,
	) -> Result<MerkleTreeCollapsedUpdate, WalletSyncError> {
		let update_bytes = hex::decode(&update_info.update_data).map_err(|e| {
			error!("[PARSE_COLLAPSED_UPDATE] Failed to decode hex: {}", e);
			WalletSyncError::MerkleTreeUpdateError(format!("Failed to decode hex: {}", e))
		})?;

		let collapsed_update: MerkleTreeCollapsedUpdate =
			deserialize(&update_bytes[..], self.network).map_err(|e| {
				error!(
					"[PARSE_COLLAPSED_UPDATE] Failed to deserialize collapsed update: {}",
					e
				);
				WalletSyncError::MerkleTreeUpdateError(format!(
					"Failed to deserialize collapsed update: {}",
					e
				))
			})?;

		Ok(collapsed_update)
	}

	/// Apply a collapsed update to a specific wallet
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
				info!(
					"Applied collapsed update: start={}, end={}, new first_free={} (was {})",
					collapsed_update.start, collapsed_update.end, new_state.first_free, wallet.state.first_free
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

	/// Apply multiple collapsed updates to a wallet
	pub fn apply_collapsed_updates(
		&self,
		wallet_seed: &WalletSeed,
		updates: &[MerkleTreeCollapsedUpdate],
	) -> Result<(), WalletSyncError> {
		for update in updates {
			self.apply_collapsed_update(wallet_seed, update)?;
		}
		Ok(())
	}

	/// Process a collapsed update info and return the parsed update
	pub async fn process_collapsed_update(
		&self,
		update_info: &CollapsedUpdateInfo,
	) -> Result<MerkleTreeCollapsedUpdate, WalletSyncError> {
		info!(
			"Processing collapsed update at blockchain index {} - start: {}, end: {}",
			update_info.blockchain_index, update_info.start, update_info.end
		);
		
		// Log current wallet state for debugging
		{
			let wallets_guard = self.context.wallets.lock().unwrap();
			if let Some(wallet) = wallets_guard.get(&WalletSeed::from("151f521307dab9fd5339aabc81542d30e2c33b0e0a1de5e80f3b343ccd72b7d4")) {
				info!(
					"Current wallet state before update: first_free={}",
					wallet.state.first_free
				);
			}
		}

		self.parse_collapsed_update(update_info).await
	}
}
