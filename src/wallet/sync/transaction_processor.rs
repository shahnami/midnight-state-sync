//! Transaction processing for wallet synchronization.
//!
//! This module provides the `TransactionProcessor`, which is responsible for parsing and validating
//! raw transaction data received during synchronization. It ensures that only valid transactions are
//! processed and applied to the wallet state.
//!
//! The transaction processor is used by the orchestrator and event handlers to decode and validate
//! transactions before they are buffered and applied.

use crate::indexer::TransactionData;
use crate::wallet::WalletSyncError;

use midnight_node_ledger_helpers::{
    DefaultDB, NetworkId, Proof, Transaction, mn_ledger_serialize::deserialize,
};
use tracing::{debug, error};

#[derive(Clone)]
/// Service for parsing and validating transactions during wallet sync.
///
/// The transaction processor decodes raw transaction data, deserializes it, and returns a parsed
/// transaction object for further processing. It is used to ensure that only valid transactions are
/// applied to the wallet state.
pub struct TransactionProcessor {
    network: NetworkId,
}

impl TransactionProcessor {
    /// Create a new transaction processor for the given network.
    pub fn new(network: NetworkId) -> Self {
        Self { network }
    }

    /// Parse raw transaction hex into a Transaction type.
    ///
    /// This method decodes and deserializes the transaction data for further processing.
    pub async fn parse_transaction(
        &self,
        raw_hex: &str,
    ) -> Result<Transaction<Proof, DefaultDB>, WalletSyncError> {
        let tx_bytes = hex::decode(raw_hex).map_err(|e| {
            error!("Failed to decode hex: {}", e);
            WalletSyncError::ParseError(format!("Failed to decode hex: {}", e))
        })?;

        let transaction: Transaction<Proof, DefaultDB> = deserialize(&tx_bytes[..], self.network)
            .map_err(|e| {
            error!("Failed to deserialize transaction: {}", e);
            WalletSyncError::ParseError(format!("Failed to deserialize transaction: {}", e))
        })?;

        Ok(transaction)
    }

    /// Process a transaction data object into a parsed transaction.
    ///
    /// Returns None if the transaction data does not contain raw data.
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
}
