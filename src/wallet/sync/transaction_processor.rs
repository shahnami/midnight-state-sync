use crate::indexer::TransactionData;
use crate::wallet::WalletSyncError;
use midnight_node_ledger_helpers::{DefaultDB, NetworkId, Proof, Transaction};
use midnight_serialize::deserialize;
use tracing::{debug, error};

#[derive(Clone)]
pub struct TransactionProcessor {
	network: NetworkId,
}

impl TransactionProcessor {
	pub fn new(network: NetworkId) -> Self {
		Self { network }
	}

	/// Parse raw transaction hex into midnight-node Transaction type
	pub async fn parse_transaction(
		&self,
		raw_hex: &str,
	) -> Result<Transaction<Proof, DefaultDB>, WalletSyncError> {
		let tx_bytes = hex::decode(raw_hex).map_err(|e| {
			error!("[PARSE_TRANSACTION] Failed to decode hex: {}", e);
			WalletSyncError::ParseError(format!("Failed to decode hex: {}", e))
		})?;

		let transaction: Transaction<Proof, DefaultDB> = deserialize(&tx_bytes[..], self.network)
			.map_err(|e| {
			error!(
				"[PARSE_TRANSACTION] Failed to deserialize transaction: {}",
				e
			);
			WalletSyncError::ParseError(format!("Failed to deserialize transaction: {}", e))
		})?;

		Ok(transaction)
	}

	/// Process a transaction data object into a parsed transaction
	pub async fn process_transaction(
		&self,
		transaction_data: &TransactionData,
	) -> Result<Option<Transaction<Proof, DefaultDB>>, WalletSyncError> {
		if let Some(raw_hex) = &transaction_data.raw {
			debug!(
				"Processing transaction: {} (apply_stage: {:?})",
				transaction_data.hash, transaction_data.apply_stage
			);

			let parsed_tx = self.parse_transaction(raw_hex).await?;
			Ok(Some(parsed_tx))
		} else {
			debug!(
				"Transaction {} has no raw data to process",
				transaction_data.hash
			);
			Ok(None)
		}
	}

	/// Batch process multiple transactions
	pub async fn process_transactions_batch(
		&self,
		transactions: &[TransactionData],
	) -> Result<Vec<Transaction<Proof, DefaultDB>>, WalletSyncError> {
		let mut processed = Vec::new();

		for tx_data in transactions {
			if let Some(parsed_tx) = self.process_transaction(tx_data).await? {
				processed.push(parsed_tx);
			}
		}

		Ok(processed)
	}
}
