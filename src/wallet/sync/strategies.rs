//! Synchronization strategies for wallet sync.
//!
//! This module defines the pluggable sync strategies used by the orchestrator to synchronize the wallet
//! with the blockchain. Strategies determine how data is fetched and which events are emitted.
//!
//! - `SyncStrategy`: Trait for all sync strategies.
//! - `RelevantTransactionSync`: Syncs only wallet-relevant transactions using a viewing key.
//! - `FullChainSync`: Syncs all blocks and transactions from the chain.
//! - `SyncConfig`: Configuration for strategies (timeouts, raw data, etc).
//!
//! Strategies interact with the event system to emit events for transactions, Merkle updates, and progress.
//! The orchestrator selects and runs the appropriate strategy based on user configuration.

use crate::indexer::{MidnightIndexerClient, ViewingKeyFormat};
use crate::wallet::WalletSyncError;
use crate::wallet::sync::events::{EventDispatcher, SyncEvent, convert_indexer_event};
use crate::wallet::sync::progress_tracker::SyncProgressTracker;

use futures_util::StreamExt;
use tracing::{debug, error, info};

/// Trait for different synchronization strategies.
///
/// A sync strategy defines how the wallet fetches and processes blockchain data. It is responsible for
/// emitting events for transactions, Merkle updates, and progress, and for driving the sync lifecycle.
#[async_trait::async_trait]
pub trait SyncStrategy: Send + Sync {
	/// Execute the sync strategy from the given start height.
	///
	/// This method should emit events via the dispatcher and update the progress tracker.
	async fn sync(
		&mut self,
		start_height: u64,
		event_dispatcher: &mut EventDispatcher,
		progress_tracker: &mut SyncProgressTracker,
	) -> Result<(), WalletSyncError>;
}

/// Configuration for sync strategies.
///
/// Controls timeouts and whether to include raw transaction data.
#[derive(Debug, Clone)]
pub struct SyncConfig {
	/// Timeout for idle periods (no new events).
	pub idle_timeout: tokio::time::Duration,
	/// Whether to include raw transaction data in events.
	pub include_raw_data: bool,
}

impl Default for SyncConfig {
	fn default() -> Self {
		Self {
			idle_timeout: tokio::time::Duration::from_secs(5),
			include_raw_data: true,
		}
	}
}

/// Strategy for syncing only wallet-relevant transactions.
///
/// Uses a viewing key to subscribe to relevant events from the indexer, minimizing data transfer and processing.
pub struct RelevantTransactionSync {
	indexer_client: MidnightIndexerClient,
	viewing_key: ViewingKeyFormat,
	config: SyncConfig,
}

impl RelevantTransactionSync {
	/// Create a new relevant transaction sync strategy.
	pub fn new(
		indexer_client: MidnightIndexerClient,
		viewing_key: ViewingKeyFormat,
		config: SyncConfig,
	) -> Self {
		Self {
			indexer_client,
			viewing_key,
			config,
		}
	}

	/// Establish a session with the indexer for the wallet's viewing key.
	async fn establish_session(&self) -> Result<String, WalletSyncError> {
		let session_id = self
			.indexer_client
			.connect_wallet(&self.viewing_key)
			.await
			.map_err(|e| {
				WalletSyncError::SessionError(format!("Failed to connect wallet: {}", e))
			})?;

		debug!("Established wallet session: {}", session_id);
		Ok(session_id)
	}
}

#[async_trait::async_trait]
impl SyncStrategy for RelevantTransactionSync {
	/// Execute the relevant transaction sync strategy.
	async fn sync(
		&mut self,
		start_height: u64,
		event_dispatcher: &mut EventDispatcher,
		progress_tracker: &mut SyncProgressTracker,
	) -> Result<(), WalletSyncError> {
		let session_id = self.establish_session().await?;

		debug!(
			"Starting relevant transaction sync from index {} using session {}",
			start_height, session_id
		);

		// Subscribe to wallet events
		let mut wallet_stream = self
			.indexer_client
			.subscribe_wallet(
				&session_id,
				Some(start_height),
				Some(self.config.include_raw_data),
			)
			.await?;

		let mut last_event_time = tokio::time::Instant::now();
		let mut received_initial_progress = false;

		loop {
			let timeout = tokio::time::sleep_until(last_event_time + self.config.idle_timeout);
			tokio::pin!(timeout);

			tokio::select! {
				Some(event_result) = wallet_stream.next() => {
					last_event_time = tokio::time::Instant::now();

					match event_result {
						Ok(indexer_event) => {
							debug!("Processing indexer event: {:#?}", indexer_event);

							// Convert and dispatch events
							let sync_events = convert_indexer_event(indexer_event);
							for event in sync_events {
								// Update progress tracker based on event type
								match &event {
									SyncEvent::TransactionReceived { blockchain_index, .. } => {
										progress_tracker.record_transaction(*blockchain_index);
									}
									SyncEvent::MerkleUpdateReceived { blockchain_index, .. } => {
										progress_tracker.record_merkle_update(*blockchain_index);
									}
									SyncEvent::ProgressUpdate {
										highest_index,
										highest_relevant_wallet_index,
										..
									} => {
										received_initial_progress = true;

										// If we're starting from a height that's already at or past the highest relevant index,
										// and we haven't processed any new data, we're already synced
										if start_height >= *highest_relevant_wallet_index && !progress_tracker.get_stats().has_processed_data {
											debug!("Already synced to latest state (start height {} >= highest relevant {})",
												start_height, highest_relevant_wallet_index);

											// Dispatch completion event
											event_dispatcher.dispatch(&SyncEvent::SyncCompleted).await?;

											return Ok(());
										}

										if progress_tracker.is_sync_complete(*highest_index, *highest_relevant_wallet_index) {
											debug!("Sync completed based on progress update");

											// Dispatch completion event
											event_dispatcher.dispatch(&SyncEvent::SyncCompleted).await?;

											return Ok(());
										}
									}
									_ => {}
								}

								// Process the event
								event_dispatcher.dispatch(&event).await?;
							}

							// Log progress periodically
							progress_tracker.log_progress(false);
						}
						Err(e) => {
							error!("Error in wallet subscription: {}", e);
							event_dispatcher.dispatch(&SyncEvent::SyncError).await?;
						}
					}
				}
				_ = &mut timeout => {
					// If we received an initial progress update showing we're already synced,
					// or if we started from a recent height, consider sync complete
					if received_initial_progress || start_height > 0 {
						debug!("No new events for {} seconds, sync is complete",
							  self.config.idle_timeout.as_secs());
					} else {
						debug!("No events received within timeout, possible connectivity issue");
					}
					break;
				}
			}
		}

		// Validate completion - skip validation if we're already at the latest state
		let stats = progress_tracker.get_stats();
		if stats.has_processed_data {
			if let Err(e) = progress_tracker.validate_completion() {
				return Err(WalletSyncError::SyncError(e));
			}
		} else if start_height == 0 {
			// Only error if we started from genesis and got no data
			return Err(WalletSyncError::SyncError(
				"No data received during sync from genesis".to_string(),
			));
		}

		// Dispatch final completion event
		let stats = progress_tracker.get_stats();
		info!("Sync completed: {}", stats.summary());

		event_dispatcher.dispatch(&SyncEvent::SyncCompleted).await?;

		Ok(())
	}
}
