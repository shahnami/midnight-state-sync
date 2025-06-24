use crate::indexer::{MidnightIndexerClient, ViewingKeyFormat};
use crate::wallet::WalletSyncError;
use crate::wallet::sync::events::{EventDispatcher, SyncEvent, convert_indexer_event};
use crate::wallet::sync::progress_tracker::SyncProgressTracker;
use futures_util::StreamExt;
use tracing::{debug, error, info};

/// Trait for different synchronization strategies
#[async_trait::async_trait]
pub trait SyncStrategy: Send + Sync {
	/// Execute the sync strategy
	async fn sync(
		&mut self,
		start_height: u64,
		event_dispatcher: &mut EventDispatcher,
		progress_tracker: &mut SyncProgressTracker,
	) -> Result<(), WalletSyncError>;

	/// Get the name of this strategy
	fn name(&self) -> &'static str;
}

/// Configuration for sync strategies
#[derive(Debug, Clone)]
pub struct SyncConfig {
	/// Timeout for idle periods (no new events)
	pub idle_timeout: tokio::time::Duration,
	/// Whether to include raw transaction data
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

/// Strategy for syncing only wallet-relevant transactions
pub struct RelevantTransactionSync {
	indexer_client: MidnightIndexerClient,
	viewing_key: ViewingKeyFormat,
	config: SyncConfig,
}

impl RelevantTransactionSync {
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

	async fn establish_session(&self) -> Result<String, WalletSyncError> {
		let session_id = self
			.indexer_client
			.connect_wallet(&self.viewing_key)
			.await
			.map_err(|e| {
				WalletSyncError::SessionError(format!("Failed to connect wallet: {}", e))
			})?;

		info!("Established wallet session: {}", session_id);
		Ok(session_id)
	}
}

#[async_trait::async_trait]
impl SyncStrategy for RelevantTransactionSync {
	async fn sync(
		&mut self,
		start_height: u64,
		event_dispatcher: &mut EventDispatcher,
		progress_tracker: &mut SyncProgressTracker,
	) -> Result<(), WalletSyncError> {
		let session_id = self.establish_session().await?;

		info!(
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
											info!("Already synced to latest state (start height {} >= highest relevant {})", 
												start_height, highest_relevant_wallet_index);
											
											// Dispatch completion event
											event_dispatcher.dispatch(&SyncEvent::SyncCompleted {
												final_height: start_height,
												transactions_processed: 0,
											}).await?;
											
											return Ok(());
										}
										
										if progress_tracker.is_sync_complete(*highest_index, *highest_relevant_wallet_index) {
											info!("Sync completed based on progress update");

											// Dispatch completion event
											let stats = progress_tracker.get_stats();
											event_dispatcher.dispatch(&SyncEvent::SyncCompleted {
												final_height: stats.highest_processed_index,
												transactions_processed: stats.transactions_processed,
											}).await?;

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
							event_dispatcher.dispatch(&SyncEvent::SyncError {
								error: e.to_string(),
								recoverable: true,
							}).await?;
						}
					}
				}
				_ = &mut timeout => {
					// If we received an initial progress update showing we're already synced, 
					// or if we started from a recent height, consider sync complete
					if received_initial_progress || start_height > 0 {
						info!("No new events for {} seconds, sync is complete",
							  self.config.idle_timeout.as_secs());
					} else {
						info!("No events received within timeout, possible connectivity issue");
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
				"No data received during sync from genesis".to_string()
			));
		}

		// Dispatch final completion event
		let stats = progress_tracker.get_stats();
		info!("Sync completed: {}", stats.summary());

		event_dispatcher
			.dispatch(&SyncEvent::SyncCompleted {
				final_height: stats.highest_processed_index,
				transactions_processed: stats.transactions_processed,
			})
			.await?;

		Ok(())
	}

	fn name(&self) -> &'static str {
		"RelevantTransactionSync"
	}
}

/// Strategy for syncing the entire blockchain
pub struct FullChainSync {
	indexer_client: MidnightIndexerClient,
	config: SyncConfig,
}

impl FullChainSync {
	pub fn new(indexer_client: MidnightIndexerClient, config: SyncConfig) -> Self {
		Self {
			indexer_client,
			config,
		}
	}
}

#[async_trait::async_trait]
impl SyncStrategy for FullChainSync {
	async fn sync(
		&mut self,
		start_height: u64,
		event_dispatcher: &mut EventDispatcher,
		progress_tracker: &mut SyncProgressTracker,
	) -> Result<(), WalletSyncError> {
		info!("Starting full chain sync from block {}", start_height);

		// Subscribe to ALL blocks
		let mut blocks_stream = self.indexer_client.subscribe_blocks(start_height).await?;
		let mut last_event_time = tokio::time::Instant::now();

		// Use shorter timeout for block subscription
		const BLOCK_IDLE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(4);

		loop {
			let timeout = tokio::time::sleep_until(last_event_time + BLOCK_IDLE_TIMEOUT);
			tokio::pin!(timeout);

			tokio::select! {
				Some(block_result) = blocks_stream.next() => {
					last_event_time = tokio::time::Instant::now();

					match block_result {
						Ok(block_data) => {
							if let Some(block_obj) = block_data.as_object() {
								let height = block_obj.get("height")
									.and_then(|h| h.as_u64())
									.unwrap_or(0);

								let block_hash = block_obj.get("hash")
									.and_then(|h| h.as_str())
									.unwrap_or("unknown");

								debug!("Processing block {} at height {}", block_hash, height);

								// Process all transactions in the block
								if let Some(transactions) = block_obj.get("transactions")
									.and_then(|t| t.as_array()) {

									for tx_data in transactions {
										if let Some(tx_obj) = tx_data.as_object() {
											let tx_hash = tx_obj.get("hash")
												.and_then(|h| h.as_str())
												.unwrap_or("unknown");

											if let Some(raw_hex) = tx_obj.get("raw")
												.and_then(|r| r.as_str()) {

												// Create transaction data
												let transaction_data = crate::indexer::TransactionData {
													hash: tx_hash.to_string(),
													identifiers: None,
													raw: Some(raw_hex.to_string()),
													apply_stage: tx_obj.get("applyStage")
														.and_then(|s| s.as_str())
														.map(|s| s.to_string()),
													merkle_tree_root: tx_obj.get("merkleTreeRoot")
														.and_then(|r| r.as_str())
														.map(|s| s.to_string()),
													protocol_version: tx_obj.get("protocolVersion")
														.and_then(|v| v.as_u64())
														.map(|v| v as u32),
												};

												// Dispatch transaction event
												event_dispatcher.dispatch(&SyncEvent::TransactionReceived {
													blockchain_index: height,
													transaction_data,
												}).await?;

												progress_tracker.record_transaction(height);
											}
										}
									}
								}

								// Log progress periodically
								progress_tracker.log_progress(false);
							}
						}
						Err(e) => {
							error!("Error in blocks subscription: {}", e);
							event_dispatcher.dispatch(&SyncEvent::SyncError {
								error: e.to_string(),
								recoverable: true,
							}).await?;
						}
					}
				}
				_ = &mut timeout => {
					info!("No new blocks for {} seconds, stopping sync", BLOCK_IDLE_TIMEOUT.as_secs());
					break;
				}
			}
		}

		// Validate completion
		if let Err(e) = progress_tracker.validate_completion() {
			return Err(WalletSyncError::SyncError(e));
		}

		// Dispatch completion event
		let stats = progress_tracker.get_stats();
		info!("Full chain sync completed: {}", stats.summary());

		event_dispatcher
			.dispatch(&SyncEvent::SyncCompleted {
				final_height: stats.highest_processed_index,
				transactions_processed: stats.transactions_processed,
			})
			.await?;

		Ok(())
	}

	fn name(&self) -> &'static str {
		"FullChainSync"
	}
}
