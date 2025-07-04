//! Wallet sync orchestrator and integration point for all sync services.
//!
//! This module defines the `WalletSyncOrchestrator`, which coordinates all components involved in
//! synchronizing a wallet with the blockchain. It selects and runs a sync strategy, manages event
//! dispatching, invokes transaction and Merkle update processing, and handles state persistence.
//!
//! The orchestrator is responsible for:
//! - Initializing and wiring together all sync services (transaction processor, Merkle update service, persistence, etc.)
//! - Selecting the appropriate sync strategy (full chain or relevant transactions)
//! - Managing the event dispatcher and event handler registration
//! - Buffering and applying updates in the correct order
//! - Ensuring robust, resumable, and observable synchronization
//!
//! The orchestrator uses the event system to decouple sync logic from state updates and persistence.
//! The `OrchestratorEventHandler` integrates with all services to process events and buffer updates.

use crate::indexer::{ApplyStage, MidnightIndexerClient, ViewingKeyFormat};
use crate::transaction::MIDNIGHT_TOKEN_DECIMALS;
use crate::utils::format_token_amount;
use crate::wallet::WalletSyncError;
use crate::wallet::sync::{
    events::{EventDispatcher, SyncEvent, SyncEventHandler},
    merkle_update_service::MerkleTreeUpdateService,
    progress_tracker::SyncProgressTracker,
    strategies::{RelevantTransactionSync, SyncConfig, SyncStrategy},
    transaction_processor::TransactionProcessor,
};

use bech32::{Bech32m, Hrp};
use midnight_ledger_prototype::transient_crypto::merkle_tree::MerkleTreeCollapsedUpdate;
use midnight_node_ledger_helpers::{
    DefaultDB, LedgerContext, NATIVE_TOKEN, NetworkId, Proof, Serializable, Transaction, Wallet,
    WalletSeed,
};
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

/// Enum to track updates in chronological order during wallet synchronization.
///
/// This enum is used to buffer and order both transaction and Merkle tree updates
/// as they are received from the indexer, ensuring correct application order.
///
/// - `Transaction`: Represents a transaction update with its index, transaction data, and apply stage.
/// - `MerkleUpdate`: Represents a Merkle tree update with its index and update data.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum ChronologicalUpdate {
    Transaction {
        index: u64,
        tx: Transaction<Proof, DefaultDB>,
        apply_stage: Option<ApplyStage>,
    },
    MerkleUpdate {
        index: u64,
        update: MerkleTreeCollapsedUpdate,
    },
}

/// Main wallet sync orchestrator that coordinates all sync components.
///
/// This struct is the entry point for wallet synchronization. It manages the lifecycle of the sync
/// process, wires together all services, and ensures that events are handled and state is updated
/// correctly. It is responsible for selecting the sync strategy, managing persistence, and tracking progress.
pub struct WalletSyncOrchestrator {
    context: Arc<LedgerContext<DefaultDB>>,
    seed: WalletSeed,

    // Services
    transaction_processor: TransactionProcessor,
    merkle_service: MerkleTreeUpdateService,

    // Sync strategy
    sync_strategy: Box<dyn SyncStrategy>,
}

impl WalletSyncOrchestrator {
    /// Create a new orchestrator with the specified sync strategy and configuration.
    ///
    /// This method initializes all services and selects the sync strategy based on the provided options.
    pub fn new(
        indexer_client: MidnightIndexerClient,
        context: Arc<LedgerContext<DefaultDB>>,
        seed: WalletSeed,
        network: NetworkId,
    ) -> Result<Self, WalletSyncError> {
        let wallet = context.wallet_from_seed(seed);
        let viewing_key = Self::derive_viewing_key(&wallet, network)?;

        // Create services
        let transaction_processor = TransactionProcessor::new(network);
        let merkle_service = MerkleTreeUpdateService::new(context.clone(), network);

        // Create sync strategy
        let sync_strategy: Box<dyn SyncStrategy> = Box::new(RelevantTransactionSync::new(
            indexer_client,
            viewing_key.clone(),
            SyncConfig::default(),
        ));

        Ok(Self {
            context,
            seed,
            transaction_processor,
            merkle_service,
            sync_strategy,
        })
    }

    /// Derive viewing key from wallet for the specified network.
    ///
    /// Used internally to generate the viewing key for relevant transaction sync.
    fn derive_viewing_key(
        wallet: &Wallet<DefaultDB>,
        network: NetworkId,
    ) -> Result<ViewingKeyFormat, WalletSyncError> {
        let secret_keys = &wallet.secret_keys;
        let enc_secret_key = &secret_keys.encryption_secret_key;
        let mut enc_secret_bytes = Vec::new();
        Serializable::serialize(enc_secret_key, &mut enc_secret_bytes).map_err(|e| {
            WalletSyncError::ViewingKeyError(format!(
                "Failed to serialize encryption secret key: {}",
                e
            ))
        })?;

        let network_suffix = match network {
            NetworkId::MainNet => "",
            NetworkId::TestNet => "_test",
            NetworkId::DevNet => "_dev",
            NetworkId::Undeployed => "_undeployed",
            _ => "",
        };

        let hrp_str = format!("mn_shield-esk{}", network_suffix);
        let hrp = Hrp::parse(&hrp_str).map_err(|e| {
            WalletSyncError::ViewingKeyError(format!("Invalid HRP for viewing key: {}", e))
        })?;

        let viewing_key_bech32 =
            bech32::encode::<Bech32m>(hrp, &enc_secret_bytes).map_err(|e| {
                WalletSyncError::ViewingKeyError(format!(
                    "Failed to encode viewing key in Bech32m: {}",
                    e
                ))
            })?;

        Ok(ViewingKeyFormat::Bech32m(viewing_key_bech32))
    }

    /// Start synchronization.
    ///
    /// This method runs the sync strategy, dispatches events, buffers updates, and applies them in order.
    /// It also manages state persistence and progress tracking.
    pub async fn sync(&mut self) -> Result<(), WalletSyncError> {
        info!("Starting wallet synchronization");

        // Start from genesis
        let start_height = 0;
        info!("Starting sync from genesis");

        // Create progress tracker
        let mut progress_tracker = SyncProgressTracker::new(start_height);

        // Create shared buffer for chronological updates
        let updates_buffer = Arc::new(Mutex::new(Vec::<ChronologicalUpdate>::new()));

        // Execute sync strategy with a custom event handler
        let event_handler = OrchestratorEventHandler {
            transaction_processor: self.transaction_processor.clone(),
            merkle_service: self.merkle_service.clone(),
            updates_buffer: updates_buffer.clone(),
        };

        let mut event_dispatcher = EventDispatcher::new();
        event_dispatcher.register_handler(Box::new(event_handler));

        self.sync_strategy
            .sync(start_height, &mut event_dispatcher, &mut progress_tracker)
            .await?;

        // Apply all buffered updates in chronological order
        info!("Applying buffered updates in chronological order");

        let mut updates = updates_buffer.lock().unwrap().clone();
        // Sort by blockchain index to ensure chronological order
        updates.sort_by_key(|update| match update {
            ChronologicalUpdate::Transaction { index, .. } => *index,
            ChronologicalUpdate::MerkleUpdate { index, .. } => *index,
        });

        info!("Applying {} updates in chronological order", updates.len());

        for update in updates {
            match update {
                ChronologicalUpdate::MerkleUpdate { index, update } => {
                    info!("Applying merkle update at index {}", index);
                    self.merkle_service
                        .apply_collapsed_update(&self.seed, &update)?;
                }
                ChronologicalUpdate::Transaction {
                    index,
                    tx,
                    apply_stage,
                } => {
                    // Only apply transactions that succeeded
                    let should_apply = match &apply_stage {
                        Some(stage) => {
                            if stage.should_apply() {
                                true
                            } else {
                                info!(
                                    "Skipping transaction at index {} with apply_stage: {:?}",
                                    index, stage
                                );
                                false
                            }
                        }
                        None => {
                            info!(
                                "Transaction at index {} has no apply_stage, applying anyway",
                                index
                            );
                            true
                        }
                    };

                    if should_apply {
                        info!("Applying transaction at index {}", index);
                        self.context.update_from_txs(&[tx]);
                    }
                }
            }
        }

        info!("Wallet synchronization completed successfully");
        Ok(())
    }

    /// Get the current balance from the wallet.
    ///
    /// This method iterates over all coins in the wallet and sums the value of native tokens.
    pub async fn get_current_balance(&self) -> u128 {
        let wallet = self.context.wallet_from_seed(self.seed);
        let total_coins = wallet.state.coins.iter().count();
        debug!("Wallet has {} total coins", total_coins);

        let mut balance = 0u128;

        for (idx, (nullifier, qualified_coin_info)) in wallet.state.coins.iter().enumerate() {
            let coin_info: midnight_node_ledger_helpers::CoinInfo = (&*qualified_coin_info).into();

            debug!(
                "Coin {}: type={:?}, value={} tDUST, nullifier={:?}",
                idx + 1,
                coin_info.type_,
                format_token_amount(coin_info.value, MIDNIGHT_TOKEN_DECIMALS),
                nullifier
            );

            if coin_info.type_ == NATIVE_TOKEN {
                debug!(
                    "Adding native token coin with value: {} tDUST",
                    format_token_amount(coin_info.value, MIDNIGHT_TOKEN_DECIMALS)
                );
                balance = balance.saturating_add(coin_info.value);
            } else {
                debug!(
                    "Skipping non-native token coin (type: {:?})",
                    coin_info.type_
                );
            }
        }

        balance
    }
}

/// Event handler that integrates with the orchestrator's services.
///
/// This handler receives all sync events and is responsible for:
/// - Processing and buffering transactions and Merkle updates
/// - Ensuring updates are applied in the correct order after sync completes
///
/// It interacts with the transaction processor and Merkle update service.
struct OrchestratorEventHandler {
    transaction_processor: TransactionProcessor,
    merkle_service: MerkleTreeUpdateService,
    // Buffer for accumulating updates in chronological order
    updates_buffer: Arc<Mutex<Vec<ChronologicalUpdate>>>,
}

#[async_trait::async_trait]
impl SyncEventHandler for OrchestratorEventHandler {
    /// Handle a sync event by processing and buffering updates, and managing persistence.
    async fn handle(&mut self, event: &SyncEvent) -> Result<(), WalletSyncError> {
        match event {
            SyncEvent::TransactionReceived {
                blockchain_index,
                transaction_data,
            } => {
                // Process transaction and buffer it (don't apply yet)
                if let Some(tx) = self
                    .transaction_processor
                    .process_transaction(transaction_data)
                    .await?
                {
                    // Buffer the transaction for later application with its apply stage
                    self.updates_buffer
                        .lock()
                        .unwrap()
                        .push(ChronologicalUpdate::Transaction {
                            index: *blockchain_index,
                            tx,
                            apply_stage: transaction_data.apply_stage.clone(),
                        });
                }
            }
            SyncEvent::MerkleUpdateReceived {
                update_info,
                blockchain_index,
            } => {
                // Process and buffer merkle update (don't apply yet)
                let update = self
                    .merkle_service
                    .process_collapsed_update(update_info)
                    .await?;

                // Buffer the update for later application
                self.updates_buffer
                    .lock()
                    .unwrap()
                    .push(ChronologicalUpdate::MerkleUpdate {
                        index: *blockchain_index,
                        update,
                    });
            }
            SyncEvent::SyncCompleted => {
                // Sync completed, no additional processing needed
            }
            _ => {}
        }
        Ok(())
    }

    /// Get the name of this handler for logging and diagnostics.
    fn name(&self) -> &'static str {
        "OrchestratorEventHandler"
    }
}
