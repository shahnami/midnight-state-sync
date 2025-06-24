//! Midnight transaction builder
//!
//! This module provides a builder pattern for constructing Midnight transactions
//! following the midnight-node patterns.
//!
//! IMPORTANT: When creating transactions, always include a change output back to the sender
//! if there's any remaining value after accounting for the recipient amount and fees.
//! This ensures the indexer recognizes the transaction as relevant to the sender's wallet
//! during synchronization.

use midnight_node_ledger_helpers::{
    DB, FromContext, IntentInfo, LedgerContext, OfferInfo, Proof, ProofProvider,
    StandardTrasactionInfo as MidnightStandardTransactionInfo, Transaction, WellFormedStrictness,
};

use serde::Serialize;
use std::sync::Arc;
use thiserror::Error;
use tracing::{error, info};

#[derive(Error, Debug, Serialize)]
pub enum TransactionError {
    #[error("Transaction validation error: {0}")]
    ValidationError(String),

    #[error("Unexpected error: {0}")]
    UnexpectedError(String),

    #[error("Insufficient balance: {0}")]
    InsufficientBalance(String),
}

/// Builder for constructing Midnight transactions using midnight-node patterns
pub struct MidnightTransactionBuilder<D: DB> {
    /// The ledger context containing wallet and network information
    context: Option<Arc<LedgerContext<D>>>,
    /// The proof provider for generating ZK proofs
    proof_provider: Option<Box<dyn ProofProvider<D>>>,
    /// Random seed for transaction building
    rng_seed: Option<[u8; 32]>,
    /// The guaranteed offer to be added
    guaranteed_offer: Option<OfferInfo<D>>,
    /// Intent info containing all fallible offers with segment information preserved
    intent_info: Option<IntentInfo<D>>,
}

impl<D: DB> MidnightTransactionBuilder<D> {
    /// Creates a new transaction builder
    pub fn new() -> Self {
        Self {
            context: None,
            proof_provider: None,
            rng_seed: None,
            guaranteed_offer: None,
            intent_info: None,
        }
    }

    /// Sets the ledger context
    pub fn with_context(mut self, context: std::sync::Arc<LedgerContext<D>>) -> Self {
        self.context = Some(context);
        self
    }

    /// Sets the proof provider
    pub fn with_proof_provider(mut self, proof_provider: Box<dyn ProofProvider<D>>) -> Self {
        self.proof_provider = Some(proof_provider);
        self
    }

    /// Sets the RNG seed
    pub fn with_rng_seed(mut self, seed: [u8; 32]) -> Self {
        self.rng_seed = Some(seed);
        self
    }

    /// Sets the entire guaranteed offer
    pub fn with_guaranteed_offer(mut self, offer: OfferInfo<D>) -> Self {
        self.guaranteed_offer = Some(offer);
        self
    }

    /// Builds the final transaction
    pub async fn build(self) -> Result<Transaction<Proof, D>, TransactionError> {
        info!("Starting transaction build process");

        let context_arc = self.context.unwrap();

        let proof_provider = self.proof_provider.unwrap();
        let rng_seed = self.rng_seed.unwrap();

        // Create StandardTrasactionInfo with the context
        info!("Creating StandardTransactionInfo with context...");
        let mut tx_info = MidnightStandardTransactionInfo::new_from_context(
            context_arc.clone(),
            proof_provider.into(),
            Some(rng_seed),
        );
        info!("StandardTransactionInfo created successfully");

        // Set the guaranteed offer if present
        if let Some(offer) = self.guaranteed_offer {
            info!("Setting guaranteed offer...");
            tx_info.set_guaranteed_coins(offer);
            info!("Guaranteed offer set successfully");
        }

        // Set the intent info if present to preserve segment information
        if let Some(intent) = self.intent_info {
            info!("Setting intent info...");
            tx_info.set_intents(vec![intent]);
            info!("Intent info set successfully");
        }

        // Build the transaction
        info!("Building transaction...");
        let built_tx = tx_info.build().await;
        info!("Built transaction: {:#?}", built_tx);

        // Generate proofs
        info!("Starting proof generation...");
        let proven_tx = tx_info.prove().await;
        info!("Proof generation completed successfully");
        info!("Prove transaction: {:#?}", proven_tx);

        // Validate the proven transaction with well_formed check
        info!("Validating proven transaction with well_formed check...");

        // Get the ledger state from the context (it's wrapped in a Mutex)
        let ledger_state_guard = context_arc.ledger_state.lock().unwrap();

        let ref_state = &*ledger_state_guard;

        // Perform well_formed validation
        proven_tx
            .well_formed(ref_state, WellFormedStrictness::default())
            .map_err(|e| {
                error!("Transaction failed well_formed validation: {:?}", e);
                TransactionError::ValidationError(format!(
                    "Transaction failed well_formed validation: {:?}",
                    e
                ))
            })?;

        info!("Transaction passed well_formed validation");
        Ok(proven_tx)
    }
}

impl<D: DB> Default for MidnightTransactionBuilder<D> {
    fn default() -> Self {
        Self::new()
    }
}
