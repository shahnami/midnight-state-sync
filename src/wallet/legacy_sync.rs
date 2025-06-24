use crate::indexer::{
	MidnightIndexerClient, TransactionData, ViewingKeyFormat, WalletSyncEvent,
	ZswapChainStateUpdate,
};
use crate::transaction::MIDNIGHT_TOKEN_DECIMALS;
use crate::utils::format_token_amount;
use crate::wallet::WalletSyncError;
use crate::wallet::types::CollapsedUpdateInfo;

use bech32::{Bech32m, Hrp};
use futures_util::StreamExt;
use midnight_ledger_prototype::transient_crypto::merkle_tree::MerkleTreeCollapsedUpdate;
use midnight_node_ledger_helpers::{
	DefaultDB, LedgerContext, LedgerState, NATIVE_TOKEN, NetworkId, Proof, Serializable, 
	Transaction, Wallet, WalletSeed, WalletState,
};
use midnight_serialize::{deserialize, Deserializable};
use std::io::Cursor;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub struct MidnightWalletSyncService {
	indexer_client: MidnightIndexerClient,
	context: Arc<LedgerContext<DefaultDB>>,
	seed: WalletSeed,
	viewing_key: ViewingKeyFormat,
	/// Store processed transactions for LedgerContext synchronization
	processed_transactions: Vec<Transaction<Proof, DefaultDB>>,
	/// Store raw hex
	processed_transactions_raw: Vec<String>,
	/// Store collapsed updates for LedgerContext merkle tree synchronization
	collapsed_updates: Vec<MerkleTreeCollapsedUpdate>,
	network: NetworkId,
	/// Track the highest blockchain index we've actually processed data for
	highest_processed_index: u64,
	/// Track if we've seen any data events (to distinguish from metadata-only sync)
	has_processed_data: bool,
	/// Track all blockchain indices we've processed to detect gaps
	processed_indices: std::collections::HashSet<u64>,
}

impl MidnightWalletSyncService {
	/// Create a new session-based wallet sync service
	pub async fn new(
		indexer_client: MidnightIndexerClient,
		context: Arc<LedgerContext<DefaultDB>>,
		seed: WalletSeed,
		network: NetworkId,
	) -> Result<Self, WalletSyncError> {
		let wallet = context.wallet_from_seed(seed);
		// Derive viewing key from wallet seed for testnet (configurable in future)
		let viewing_key = Self::derive_viewing_key_from_seed_for_network(&wallet, network)?;

		Ok(Self {
			indexer_client,
			context,
			seed,
			viewing_key,
			processed_transactions: Vec::new(),
			processed_transactions_raw: Vec::new(),
			collapsed_updates: Vec::new(),
			network,
			highest_processed_index: 0,
			has_processed_data: false,
			processed_indices: std::collections::HashSet::new(),
		})
	}

	/// Derive viewing key from wallet seed for a specific network
	fn derive_viewing_key_from_seed_for_network(
		wallet: &Wallet<DefaultDB>,
		network: NetworkId,
	) -> Result<ViewingKeyFormat, WalletSyncError> {
		let secret_keys = &wallet.secret_keys;
		// Get the encryption secret key and serialize it
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

		// Encode in Bech32m format with the correct prefix for viewing keys
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

	/// Establish wallet session and start synchronization
	pub async fn start_session(&self) -> Result<String, WalletSyncError> {
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

	pub async fn apply_transactions(&self) -> Result<(), WalletSyncError> {
		self.context.update_from_txs(&self.processed_transactions);
		Ok(())
	}

	pub async fn apply_collapsed_updates(&self) -> Result<(), WalletSyncError> {
		let mut wallets_guard = self.context.wallets.lock().unwrap();

		let wallet = wallets_guard.get_mut(&self.seed).ok_or_else(|| {
			WalletSyncError::MerkleTreeUpdateError(format!(
				"Wallet with seed {:?} not found in context",
				self.seed
			))
		})?;

		for collapsed_update in &self.collapsed_updates {
			match wallet.state.apply_collapsed_update(collapsed_update) {
				Ok(new_state) => {
					info!(
						"Applied collapsed update: start={}, end={}, new first_free={}",
						collapsed_update.start, collapsed_update.end, new_state.first_free
					);
					wallet.update_state(new_state);
				}
				Err(e) => {
					error!("Failed to apply collapsed update: {}", e);
					return Err(WalletSyncError::MerkleTreeUpdateError(format!(
						"Failed to apply collapsed update to wallet state: {}",
						e
					)));
				}
			}
		}

		Ok(())
	}

	/// Synchronize wallet using session-based approach
	pub async fn sync_from_relevant(&mut self) -> Result<(), WalletSyncError> {
		let session_id = self.start_session().await?;
		// Just hardcode to 0 for now
		let start_index = 0;

		info!(
			"Starting wallet sync from index {} using session {}",
			start_index, session_id
		);

		// Subscribe to wallet events
		let mut wallet_stream = self
			.indexer_client
			.subscribe_wallet(&session_id, Some(start_index), Some(true))
			.await?;

		let mut events_processed = 0;
		let mut last_event_time = tokio::time::Instant::now();

		// Stop syncing after 15 seconds of inactivity
		const IDLE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(15);

		// Process wallet events
		loop {
			let timeout = tokio::time::sleep_until(last_event_time + IDLE_TIMEOUT);
			tokio::pin!(timeout);

			tokio::select! {
				Some(event_result) = wallet_stream.next() => {
					last_event_time = tokio::time::Instant::now();
			match event_result {
				Ok(event) => {
					info!("Processing event: {:#?}", event);
					match event {
						WalletSyncEvent::ViewingUpdate {
							type_name: _,
							index,
							update,
						} => {
							// Process each update in the array
							for update_item in update {
								match update_item {
									ZswapChainStateUpdate::RelevantTransaction {
										transaction,
										start,
										end,
									} => {
										// Determine the correct Merkle tree index
										info!(
											"Processing transaction - blockchain_index: {}, mt_start: {}, mt_end: {}",
											index, start, end
										);

										self.process_relevant_transaction(transaction).await?;

										events_processed += 1;
										self.highest_processed_index = self.highest_processed_index.max(index);
										self.has_processed_data = true;
										self.processed_indices.insert(index);
										debug!(
											"Processed relevant transaction at index {}",
											index
										);
									}
									ZswapChainStateUpdate::MerkleTreeCollapsedUpdate {
										protocol_version,
										start,
										end,
										update,
									} => {
										// Log Merkle tree collapsed updates for debugging MT index info
										info!(
											"Merkle tree collapsed update at blockchain index {} - protocol: {}, start: {}, end: {}, update_len: {}",
											index, protocol_version, start, end, update.len()
										);

										// Store the collapsed update for LedgerContext synchronization
										if !update.is_empty() {
											info!("Storing collapsed update for LedgerContext sync: start={}, end={}", start, end);

											let collapsed_update_info = CollapsedUpdateInfo {
												blockchain_index: index,
												protocol_version,
												start,
												end,
												update_data: update,
											};

											self.process_relevant_collapsed_update(collapsed_update_info).await?;

											self.highest_processed_index = self.highest_processed_index.max(index);
											self.has_processed_data = true;
											self.processed_indices.insert(index);
											debug!(
												"Processed relevant collapsed update at index {}",
												index
											);
										}
									}
								}
							}
						}
						WalletSyncEvent::ProgressUpdate {
							type_name: _,
							highest_index,
							highest_relevant_index,
							highest_relevant_wallet_index,
						} => {
							debug!(
								"Progress update - highest: {}, relevant: {}, wallet: {}, processed up to: {}",
								highest_index,
								highest_relevant_index,
								highest_relevant_wallet_index,
								self.highest_processed_index
							);

							// Only consider sync complete if:
							// 1. We've reached the highest relevant index
							// 2. We've actually processed data up to that point
							// 3. We've processed at least some data (not just metadata)
							if highest_index >= highest_relevant_wallet_index
								&& self.highest_processed_index >= highest_relevant_wallet_index
								&& self.has_processed_data {
								info!("Wallet sync completed - all data processed up to index {}",
									highest_relevant_wallet_index);
								break;
							} else if highest_index >= highest_relevant_wallet_index && !self.has_processed_data {
								warn!("Progress update indicates completion but no data was processed - continuing sync");
							}
						}
					}
				}
				Err(e) => {
					error!("Error in wallet subscription: {}", e);
					// Continue processing other events
				}
			}
				}
				_ = &mut timeout => {
					// No events received for IDLE_TIMEOUT duration
					info!("No new events for {} seconds, assuming sync is complete", IDLE_TIMEOUT.as_secs());
					break;
				}
			}
		}

		info!(
			"Sync completed! Processed {} events up to blockchain index {}",
			events_processed, self.highest_processed_index
		);

		// Verify we actually processed data
		if !self.has_processed_data {
			return Err(WalletSyncError::SyncError(
				"Sync completed without processing any data - possible incomplete sync".to_string(),
			));
		}

		// Check for gaps in processed indices
		if self.has_processed_data && self.processed_indices.len() > 1 {
			let mut sorted_indices: Vec<u64> = self.processed_indices.iter().cloned().collect();
			sorted_indices.sort();

			for window in sorted_indices.windows(2) {
				if window[1] - window[0] > 1 {
					warn!(
						"Gap detected in blockchain indices: missing indices between {} and {}",
						window[0], window[1]
					);
				}
			}
		}

		Ok(())
	}

	/// Process a relevant transaction delivered by the indexer
	async fn process_relevant_transaction(
		&mut self,
		transaction_data: TransactionData,
	) -> Result<(), WalletSyncError> {
		if let Some(raw_hex) = &transaction_data.raw {
			// Parse transaction into midnight-node format
			let parsed_tx = Self::parse_transaction(raw_hex, self.network).await?;

			// Store the transaction for LedgerContext synchronization
			self.processed_transactions.push(parsed_tx.clone());

			info!(
				"Successfully stored relevant transaction: {} (apply_stage: {})",
				transaction_data.hash,
				transaction_data.apply_stage.as_ref().unwrap()
			);
		} else {
			warn!(
				"Transaction {} has no raw data to process",
				transaction_data.hash
			);
		}

		Ok(())
	}

	/// Process a relevant transaction delivered by the indexer
	async fn process_relevant_collapsed_update(
		&mut self,
		collapsed_update_info: CollapsedUpdateInfo,
	) -> Result<(), WalletSyncError> {
		// Parse transaction into midnight-node format
		let collapsed_update =
			Self::parse_collapsed_update(&collapsed_update_info, self.network).await?;

		// Store the update in memory
		self.collapsed_updates.push(collapsed_update);

		info!(
			"Successfully stored relevant collapsed update: {}",
			collapsed_update_info.blockchain_index,
		);

		Ok(())
	}

	/// Parse raw transaction hex into midnight-node Transaction type
	async fn parse_transaction(
		raw_hex: &str,
		network_id: NetworkId,
	) -> Result<Transaction<Proof, DefaultDB>, WalletSyncError> {
		// The indexer provides the transaction as a hex-encoded string
		// We need to deserialize it into the actual Transaction object
		let tx_bytes = hex::decode(raw_hex).map_err(|e| {
			error!("[PARSE_TRANSACTION] Failed to decode hex: {}", e);
			WalletSyncError::ParseError(format!("Failed to decode hex: {}", e))
		})?;

		let transaction: Transaction<Proof, DefaultDB> = deserialize(&tx_bytes[..], network_id)
			.map_err(|e| {
				error!(
					"[PARSE_TRANSACTION] Failed to deserialize transaction: {}",
					e
				);
				WalletSyncError::ParseError(format!("Failed to deserialize transaction: {}", e))
			})?;

		Ok(transaction)
	}

	/// Parse raw collapsed update hex into midnight-node MerkleTreeCollapsedUpdate type
	async fn parse_collapsed_update(
		update_info: &CollapsedUpdateInfo,
		network_id: NetworkId,
	) -> Result<MerkleTreeCollapsedUpdate, WalletSyncError> {
		// The indexer provides the update as a hex-encoded string
		// We need to deserialize it into the actual MerkleTreeCollapsedUpdate object
		let update_bytes = hex::decode(&update_info.update_data).map_err(|e| {
			error!("[PARSE_COLLAPSED_UPDATE] Failed to decode hex: {}", e);
			WalletSyncError::MerkleTreeUpdateError(format!("Failed to decode hex: {}", e))
		})?;

		let collapsed_update: midnight_ledger_prototype::transient_crypto::merkle_tree::MerkleTreeCollapsedUpdate = deserialize(&update_bytes[..], network_id).map_err(|e| {
			error!("[PARSE_COLLAPSED_UPDATE] Failed to deserialize collapsed update: {}", e);
			WalletSyncError::MerkleTreeUpdateError(format!(
				"Failed to deserialize collapsed update: {}",
				e
			))
		})?;

		Ok(collapsed_update)
	}

	/// Get the current blockchain height from the indexer
	async fn get_current_chain_height(&self) -> Result<u64, WalletSyncError> {
		// Query the indexer for the latest block height
		let query = r#"
			query Block {
				block {
					height
				}
			}
		"#;

		let response = self
			.indexer_client
			.execute_query(query, None)
			.await
			.map_err(WalletSyncError::IndexerError)?;

		// Extract the height from the response
		if let Some(data) = response.get("data") {
			if let Some(block) = data.get("block") {
				if let Some(height) = block.get("height").and_then(|h| h.as_u64()) {
					return Ok(height);
				}
			}
		}

		Err(WalletSyncError::ParseError(
			"Failed to get current chain height".to_string(),
		))
	}

	/// Load transactions from a checkpoint file
	fn load_checkpoint(&mut self, checkpoint_path: &str) -> Result<u64, WalletSyncError> {
		info!("Loading checkpoint from: {}", checkpoint_path);

		let file = std::fs::File::open(checkpoint_path).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to open checkpoint file: {}", e))
		})?;

		let transactions: Vec<String> = serde_json::from_reader(file).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to parse checkpoint file: {}", e))
		})?;

		// Extract height from filename (format: checkpoint_transactions_height_XXXXX.json)
		let height = checkpoint_path
			.strip_prefix("checkpoint_transactions_height_")
			.and_then(|s| s.strip_suffix(".json"))
			.and_then(|s| s.parse::<u64>().ok())
			.ok_or_else(|| {
				WalletSyncError::ParseError("Invalid checkpoint filename format".to_string())
			})?;

		self.processed_transactions_raw = transactions;
		self.highest_processed_index = height;

		info!(
			"Loaded {} transactions from checkpoint at height {}",
			self.processed_transactions_raw.len(),
			height
		);

		Ok(height)
	}

	/// Find the latest checkpoint file in the data directory
	fn find_latest_checkpoint() -> Option<(String, u64)> {
		std::fs::read_dir("data")
			.ok()?
			.filter_map(|entry| entry.ok())
			.filter_map(|entry| {
				let path = entry.path();
				let filename = path.file_name()?.to_str()?;

				if filename.starts_with("checkpoint_transactions_height_")
					&& filename.ends_with(".json")
				{
					let height = filename
						.strip_prefix("checkpoint_transactions_height_")
						.and_then(|s| s.strip_suffix(".json"))
						.and_then(|s| s.parse::<u64>().ok())?;

					Some((path.to_str()?.to_string(), height))
				} else {
					None
				}
			})
			.max_by_key(|(_, height)| *height)
	}

	/// Try to resume sync from the latest checkpoint
	/// Returns the starting height (0 if no checkpoint found)
	fn try_resume_from_checkpoint(&mut self) -> Result<u64, WalletSyncError> {
		if let Some((checkpoint_file, height)) = Self::find_latest_checkpoint() {
			info!(
				"Found checkpoint at height {}, attempting to resume...",
				height
			);
			match self.load_checkpoint(&checkpoint_file) {
				Ok(loaded_height) => {
					info!(
						"Successfully resumed from checkpoint at height {} with {} raw transactions",
						loaded_height,
						self.processed_transactions_raw.len()
					);
					// Return the next height to start syncing from
					Ok(loaded_height + 1)
				}
				Err(e) => {
					warn!("Failed to load checkpoint: {}. Starting from genesis.", e);
					Ok(0)
				}
			}
		} else {
			info!("No checkpoint found, starting from genesis");
			Ok(0)
		}
	}

	/// Sync from genesis by applying ALL transactions regardless of relevancy
	/// This is the most secure approach but resource intensive
	///
	/// The sync will process all blocks from genesis up to the current chain height
	/// at the time of starting the sync. New blocks produced during the sync are
	/// not processed to ensure the sync can complete.
	/// Save a checkpoint of processed transactions to disk
	/// This allows resuming sync from a specific height in case of interruption
	fn save_checkpoint(&self, height: u64) -> Result<(), WalletSyncError> {
		let checkpoint_file_path = self.get_checkpoint_filename(height);

		match std::fs::File::create(&checkpoint_file_path) {
			Ok(file) => {
				match serde_json::to_writer_pretty(file, &self.processed_transactions_raw) {
					Ok(_) => {
						info!(
							"Checkpoint saved: {} transactions written to {}",
							self.processed_transactions_raw.len(),
							checkpoint_file_path
						);
						Ok(())
					}
					Err(e) => {
						let error_msg = format!("Failed to write checkpoint file: {}", e);
						error!("{}", error_msg);
						Err(WalletSyncError::ParseError(error_msg))
					}
				}
			}
			Err(e) => {
				let error_msg = format!("Failed to create checkpoint file: {}", e);
				error!("{}", error_msg);
				Err(WalletSyncError::ParseError(error_msg))
			}
		}
	}

	/// Generate checkpoint filename for a given height
	fn get_checkpoint_filename(&self, height: u64) -> String {
		format!("data/checkpoint_transactions_height_{}.json", height)
	}

	/// Clean up old checkpoint files, keeping only the most recent N checkpoints
	fn cleanup_old_checkpoints(keep_count: usize) -> Result<(), WalletSyncError> {
		let mut checkpoints: Vec<(String, u64)> = std::fs::read_dir("data")
			.map_err(|e| WalletSyncError::ParseError(format!("Failed to read directory: {}", e)))?
			.filter_map(|entry| entry.ok())
			.filter_map(|entry| {
				let path = entry.path();
				let filename = path.file_name()?.to_str()?;

				if filename.starts_with("checkpoint_transactions_height_")
					&& filename.ends_with(".json")
				{
					let height = filename
						.strip_prefix("checkpoint_transactions_height_")
						.and_then(|s| s.strip_suffix(".json"))
						.and_then(|s| s.parse::<u64>().ok())?;

					Some((filename.to_string(), height))
				} else {
					None
				}
			})
			.collect();

		if checkpoints.len() <= keep_count {
			return Ok(());
		}

		// Sort by height in descending order
		checkpoints.sort_by_key(|(_, height)| std::cmp::Reverse(*height));

		// Remove old checkpoints
		for (filename, _) in checkpoints.into_iter().skip(keep_count) {
			if let Err(e) = std::fs::remove_file(&filename) {
				warn!("Failed to remove old checkpoint {}: {}", filename, e);
			} else {
				info!("Removed old checkpoint: {}", filename);
			}
		}

		Ok(())
	}

	pub async fn sync_from_genesis(&mut self) -> Result<(), WalletSyncError> {
		info!("Starting blockchain sync");

		// First, try to restore wallet and ledger states if they exist
		let restored_height = match self.restore_context() {
			Ok(Some(height)) => {
				info!("Successfully restored states up to height {}", height);
				Some(height)
			}
			Ok(None) => {
				info!("No saved states found, will sync from genesis");
				None
			}
			Err(e) => {
				info!("Could not restore context from saved states: {}", e);
				None
			}
		};

		// Determine starting height based on restored state
		let start_height = if let Some(restored_h) = restored_height {
			// If we restored state, start from the next block
			// Don't load checkpoint transactions since they're already in the restored state
			info!("Resuming sync from height {} (after restored state)", restored_h + 1);
			restored_h + 1
		} else {
			// No restored state, try to resume from checkpoint
			let checkpoint_height = self.try_resume_from_checkpoint()?;
			info!("Starting sync from height {} (from checkpoint or genesis)", checkpoint_height);
			checkpoint_height
		};

		// Subscribe to ALL blocks starting from resume height
		let mut blocks_stream = self.indexer_client.subscribe_blocks(start_height).await?;

		let mut blocks_processed = 0;
		let mut transactions_processed = 0;
		let mut last_event_time = tokio::time::Instant::now();

		// Stop syncing if no blocks are received for 4 seconds
		const IDLE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(4);

		// Process ALL blocks from genesis
		loop {
			let timeout = tokio::time::sleep_until(last_event_time + IDLE_TIMEOUT);
			tokio::pin!(timeout);

			tokio::select! {
				Some(block_result) = blocks_stream.next() => {
					last_event_time = tokio::time::Instant::now();
					match block_result {
						Ok(block_data) => {
							// Process the block data
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
											// Extract transaction data
											let tx_hash = tx_obj.get("hash")
												.and_then(|h| h.as_str())
												.unwrap_or("unknown");

											let apply_stage = tx_obj.get("applyStage")
												.and_then(|s| s.as_str())
												.map(|s| s.to_string());

											let merkle_tree_root = tx_obj.get("merkleTreeRoot")
												.and_then(|r| r.as_str())
												.map(|s| s.to_string());

											if let Some(raw_hex) = tx_obj.get("raw")
												.and_then(|r| r.as_str()) {

												debug!(
													"Processing transaction {} in block {} (apply_stage: {:?}, merkle_root: {:?})",
													tx_hash, height, apply_stage, merkle_tree_root
												);

												self.processed_transactions_raw.push(raw_hex.to_string());
												transactions_processed += 1;
											}
										}
									}
								}

								// Update tracking
								blocks_processed += 1;
								self.highest_processed_index = self.highest_processed_index.max(height);
								self.has_processed_data = true;
								self.processed_indices.insert(height);

								// Log progress and checkpoint every 1000 blocks
								if blocks_processed % 1000 == 0 && blocks_processed > 0 {
									info!(
										"Genesis sync progress: processed {} blocks, {} transactions up to height {}",
										blocks_processed, transactions_processed, height
									);

									// Save checkpoint (ignore errors to continue sync)
									if let Err(e) = self.save_checkpoint(height) {
										warn!("Failed to save checkpoint at height {}: {}", height, e);
									}

									// Clean up old checkpoints (keep last 5)
									if let Err(e) = Self::cleanup_old_checkpoints(5) {
										warn!("Failed to cleanup old checkpoints: {}", e);
									}
								}

							}
						}
						Err(e) => {
							error!("Error in blocks subscription: {}", e);
							// Continue processing other blocks
						}
					}
				}
				_ = &mut timeout => {
					// No blocks received for IDLE_TIMEOUT duration
					info!("No new blocks for {} seconds, stopping sync at height {}",
						IDLE_TIMEOUT.as_secs(), self.highest_processed_index);

					// Save final checkpoint at current height
					if self.highest_processed_index > 0 {
						if let Err(e) = self.save_checkpoint(self.highest_processed_index) {
							warn!("Failed to save final checkpoint: {}", e);
						}
					}
					break;
				}
			}
		}

		info!(
			"Genesis sync completed! Processed {} blocks with {} transactions up to height {}",
			blocks_processed, transactions_processed, self.highest_processed_index
		);

		// Verify we processed data
		if !self.has_processed_data {
			return Err(WalletSyncError::SyncError(
				"Genesis sync completed without processing any data".to_string(),
			));
		}

		// Check for gaps in processed indices
		if self.has_processed_data && self.processed_indices.len() > 1 {
			let mut sorted_indices: Vec<u64> = self.processed_indices.iter().cloned().collect();
			sorted_indices.sort();

			let mut has_gaps = false;
			for window in sorted_indices.windows(2) {
				if window[1] - window[0] > 1 {
					warn!(
						"Gap detected in block heights during genesis sync: missing blocks between {} and {}",
						window[0], window[1]
					);
					has_gaps = true;
				}
			}

			if has_gaps {
				warn!(
					"Genesis sync completed with gaps in block sequence - some blocks may be missing"
				);
			}
		}

		// Apply all collected transactions in batch
		// Only parse and apply if we don't have a restored state
		if restored_height.is_none() && !self.processed_transactions_raw.is_empty() {
			info!(
				"Parsing {} transactions to wallet state",
				self.processed_transactions_raw.len()
			);

			// Parse all transactions
			for raw_hex in &self.processed_transactions_raw {
				let tx = Self::parse_transaction(raw_hex, self.network).await?;
				self.processed_transactions.push(tx);
			}
		} else if restored_height.is_some() {
			info!("Skipping transaction parsing - using restored state from height {:?}", restored_height);
		}

		info!("Sync complete.");

		// Save the wallet and ledger states after successful sync
		if let Err(e) = self.save_wallet_state() {
			warn!("Failed to save wallet state after genesis sync: {}", e);
		}
		
		if let Err(e) = self.save_ledger_state() {
			warn!("Failed to save ledger state after genesis sync: {}", e);
		}

		Ok(())
	}

	pub async fn get_current_balance(&self) -> u128 {
		let wallet = self.context.wallet_from_seed(self.seed);

		let total_coins = wallet.state.coins.iter().count();
		info!("Wallet has {} total coins", total_coins);

		let mut balance = 0u128;

		for (idx, (nullifier, qualified_coin_info)) in wallet.state.coins.iter().enumerate() {
			let coin_info: midnight_node_ledger_helpers::CoinInfo = (&*qualified_coin_info).into();

			info!(
				"Coin {}: type={:?}, value={} tDUST, nullifier={:?}",
				idx + 1,
				coin_info.type_,
				format_token_amount(coin_info.value, MIDNIGHT_TOKEN_DECIMALS),
				nullifier
			);

			if coin_info.type_ == NATIVE_TOKEN {
				info!(
					"Adding native token coin with value: {} tDUST",
					format_token_amount(coin_info.value, MIDNIGHT_TOKEN_DECIMALS)
				);
				balance = balance.saturating_add(coin_info.value);
			} else {
				info!(
					"Skipping non-native token coin (type: {:?})",
					coin_info.type_
				);
			}
		}

		balance
	}

	/// Save the wallet state to a file with sync height metadata
	pub fn save_wallet_state(&self) -> Result<(), WalletSyncError> {
		let wallet = self.context.wallet_from_seed(self.seed);
		
		// Create metadata file with sync height
		let metadata = serde_json::json!({
			"sync_height": self.highest_processed_index,
			"timestamp": chrono::Utc::now().to_rfc3339(),
		});
		
		// Save metadata
		let metadata_filename = format!("data/wallet_state_{}.meta.json", hex::encode(self.seed.0));
		std::fs::write(&metadata_filename, serde_json::to_string_pretty(&metadata).unwrap()).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to write wallet state metadata: {}", e))
		})?;
		
		// Serialize the wallet state
		let mut state_bytes = Vec::new();
		Serializable::serialize(&wallet.state, &mut state_bytes).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to serialize wallet state: {}", e))
		})?;
		
		// Write to file
		let filename = format!("data/wallet_state_{}.bin", hex::encode(self.seed.0));
		std::fs::write(&filename, &state_bytes).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to write wallet state file: {}", e))
		})?;
		
		info!("Saved wallet state to {} at height {}", filename, self.highest_processed_index);
		Ok(())
	}
	
	/// Save the ledger state to a file with sync height metadata
	pub fn save_ledger_state(&self) -> Result<(), WalletSyncError> {
		// Create metadata file with sync height
		let metadata = serde_json::json!({
			"sync_height": self.highest_processed_index,
			"timestamp": chrono::Utc::now().to_rfc3339(),
		});
		
		// Save metadata
		let metadata_filename = "data/ledger_state.meta.json";
		std::fs::write(metadata_filename, serde_json::to_string_pretty(&metadata).unwrap()).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to write ledger state metadata: {}", e))
		})?;
		
		// Get the ledger state
		let ledger_state = self.context.ledger_state.lock().unwrap();
		
		// Serialize the ledger state
		let mut state_bytes = Vec::new();
		Serializable::serialize(&*ledger_state, &mut state_bytes).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to serialize ledger state: {}", e))
		})?;
		
		// Write to file
		let filename = "data/ledger_state.bin";
		std::fs::write(filename, &state_bytes).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to write ledger state file: {}", e))
		})?;
		
		info!("Saved ledger state to {} at height {}", filename, self.highest_processed_index);
		Ok(())
	}
	
	/// Load wallet state from a file
	pub fn load_wallet_state(&self) -> Result<WalletState<DefaultDB>, WalletSyncError> {
		let filename = format!("data/wallet_state_{}.bin", hex::encode(self.seed.0));
		
		// Read the file
		let state_bytes = std::fs::read(&filename).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to read wallet state file: {}", e))
		})?;
		
		// Deserialize the wallet state using versioned deserialization
		let mut cursor = Cursor::new(&state_bytes);
		let wallet_state: WalletState<DefaultDB> = Deserializable::deserialize(&mut cursor, 0).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to deserialize wallet state: {}", e))
		})?;
		
		info!("Loaded wallet state from {}", filename);
		Ok(wallet_state)
	}
	
	/// Load ledger state from a file
	pub fn load_ledger_state(&self) -> Result<LedgerState<DefaultDB>, WalletSyncError> {
		let filename = "data/ledger_state.bin";
		
		// Read the file
		let state_bytes = std::fs::read(filename).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to read ledger state file: {}", e))
		})?;
		
		// Deserialize the ledger state using versioned deserialization
		let mut cursor = Cursor::new(&state_bytes);
		let ledger_state: LedgerState<DefaultDB> = Deserializable::deserialize(&mut cursor, 0).map_err(|e| {
			WalletSyncError::ParseError(format!("Failed to deserialize ledger state: {}", e))
		})?;
		
		info!("Loaded ledger state from {}", filename);
		Ok(ledger_state)
	}
	
	/// Restore the context with saved wallet and ledger states
	/// Returns the sync height if states were successfully restored
	pub fn restore_context(&mut self) -> Result<Option<u64>, WalletSyncError> {
		let mut wallet_height = None;
		let mut ledger_height = None;
		
		// Try to load wallet state and metadata
		let wallet_meta_filename = format!("data/wallet_state_{}.meta.json", hex::encode(self.seed.0));
		if let Ok(meta_content) = std::fs::read_to_string(&wallet_meta_filename) {
			if let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&meta_content) {
				if let Some(height) = metadata.get("sync_height").and_then(|h| h.as_u64()) {
					wallet_height = Some(height);
				}
			}
		}
		
		match self.load_wallet_state() {
			Ok(wallet_state) => {
				// Update the wallet state in context
				let mut wallets_guard = self.context.wallets.lock().unwrap();
				if let Some(wallet) = wallets_guard.get_mut(&self.seed) {
					wallet.update_state(wallet_state);
					info!("Restored wallet state from file (synced up to height {:?})", wallet_height);
				} else {
					return Err(WalletSyncError::ParseError(
						"Wallet not found in context".to_string()
					));
				}
			}
			Err(e) => {
				warn!("Could not load wallet state: {}", e);
				wallet_height = None;
			}
		}
		
		// Try to load ledger state and metadata
		let ledger_meta_filename = "data/ledger_state.meta.json";
		if let Ok(meta_content) = std::fs::read_to_string(ledger_meta_filename) {
			if let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&meta_content) {
				if let Some(height) = metadata.get("sync_height").and_then(|h| h.as_u64()) {
					ledger_height = Some(height);
				}
			}
		}
		
		match self.load_ledger_state() {
			Ok(ledger_state) => {
				// Update the ledger state in context
				let mut ledger_state_guard = self.context.ledger_state.lock().unwrap();
				*ledger_state_guard = ledger_state;
				info!("Restored ledger state from file (synced up to height {:?})", ledger_height);
			}
			Err(e) => {
				warn!("Could not load ledger state: {}", e);
				ledger_height = None;
			}
		}
		
		// Return the minimum height from both states (to ensure consistency)
		// If only one state was restored, use that height
		// If neither was restored, return None
		let restored_height = match (wallet_height, ledger_height) {
			(Some(w), Some(l)) => {
				let min_height = w.min(l);
				info!("Restored states up to height {} (wallet: {}, ledger: {})", min_height, w, l);
				
				// Update our tracking to reflect the restored state
				self.highest_processed_index = min_height;
				self.has_processed_data = true;
				
				Some(min_height)
			}
			(Some(h), None) | (None, Some(h)) => {
				info!("Partially restored state up to height {}", h);
				self.highest_processed_index = h;
				self.has_processed_data = true;
				Some(h)
			}
			(None, None) => None,
		};
		
		Ok(restored_height)
	}
}
