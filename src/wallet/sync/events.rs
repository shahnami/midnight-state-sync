use crate::indexer::{TransactionData, WalletSyncEvent as IndexerEvent};
use crate::wallet::WalletSyncError;
use crate::wallet::types::CollapsedUpdateInfo;
use midnight_ledger_prototype::transient_crypto::merkle_tree::MerkleTreeCollapsedUpdate;
use midnight_node_ledger_helpers::{DefaultDB, Proof, Transaction};

/// Events that occur during wallet synchronization
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum SyncEvent {
	/// A relevant transaction was received
	TransactionReceived {
		blockchain_index: u64,
		transaction_data: TransactionData,
	},
	/// A Merkle tree update was received
	MerkleUpdateReceived {
		blockchain_index: u64,
		update_info: CollapsedUpdateInfo,
	},
	/// Progress update from the indexer
	ProgressUpdate {
		highest_index: u64,
		highest_relevant_index: u64,
		highest_relevant_wallet_index: u64,
	},
	/// A transaction was processed successfully
	TransactionProcessed {
		blockchain_index: u64,
		transaction: Transaction<Proof, DefaultDB>,
	},
	/// A Merkle update was processed successfully
	MerkleUpdateProcessed {
		blockchain_index: u64,
		update: MerkleTreeCollapsedUpdate,
	},
	/// Sync has completed
	SyncCompleted {
		final_height: u64,
		transactions_processed: usize,
	},
	/// An error occurred during sync
	SyncError { error: String, recoverable: bool },
}

/// Trait for handling sync events
#[async_trait::async_trait]
pub trait SyncEventHandler: Send + Sync {
	/// Handle a sync event
	async fn handle(&mut self, event: &SyncEvent) -> Result<(), WalletSyncError>;

	/// Get the name of this handler for logging
	fn name(&self) -> &'static str;
}

/// Event dispatcher that manages multiple handlers
pub struct EventDispatcher {
	handlers: Vec<Box<dyn SyncEventHandler>>,
}

impl EventDispatcher {
	pub fn new() -> Self {
		Self {
			handlers: Vec::new(),
		}
	}

	/// Register a new event handler
	pub fn register_handler(&mut self, handler: Box<dyn SyncEventHandler>) {
		self.handlers.push(handler);
	}

	/// Dispatch an event to all registered handlers
	pub async fn dispatch(&mut self, event: &SyncEvent) -> Result<(), WalletSyncError> {
		for handler in &mut self.handlers {
			if let Err(e) = handler.handle(event).await {
				tracing::error!("Handler {} failed to process event: {}", handler.name(), e);
				// Continue processing with other handlers
			}
		}
		Ok(())
	}
}

/// Convert indexer events to sync events
pub fn convert_indexer_event(event: IndexerEvent) -> Vec<SyncEvent> {
	let mut sync_events = Vec::new();

	match event {
		IndexerEvent::ViewingUpdate {
			type_name: _,
			index,
			update,
		} => {
			// IMPORTANT: Process merkle updates FIRST, then transactions
			// This ensures the merkle tree state is correct before transactions are applied
			
			// First, collect all merkle updates
			for update_item in &update {
				if let crate::indexer::ZswapChainStateUpdate::MerkleTreeCollapsedUpdate {
					protocol_version,
					start,
					end,
					update,
				} = update_item {
					if !update.is_empty() {
						let update_info = CollapsedUpdateInfo {
							blockchain_index: index,
							protocol_version: *protocol_version,
							start: *start,
							end: *end,
							update_data: update.clone(),
						};
						sync_events.push(SyncEvent::MerkleUpdateReceived {
							blockchain_index: index,
							update_info,
						});
					}
				}
			}
			
			// Then, collect all transactions
			for update_item in update {
				if let crate::indexer::ZswapChainStateUpdate::RelevantTransaction {
					transaction,
					start: _,
					end: _,
				} = update_item {
					sync_events.push(SyncEvent::TransactionReceived {
						blockchain_index: index,
						transaction_data: transaction,
					});
				}
			}
		}
		IndexerEvent::ProgressUpdate {
			type_name: _,
			highest_index,
			highest_relevant_index,
			highest_relevant_wallet_index,
		} => {
			sync_events.push(SyncEvent::ProgressUpdate {
				highest_index,
				highest_relevant_index,
				highest_relevant_wallet_index,
			});
		}
	}

	sync_events
}
