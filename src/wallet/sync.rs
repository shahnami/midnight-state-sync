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
	DefaultDB, LedgerContext, NATIVE_TOKEN, NetworkId, Proof, Serializable, Transaction, Wallet,
	WalletSeed,
};
use midnight_serialize::deserialize;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub struct MidnightWalletSyncService {
	indexer_client: MidnightIndexerClient,
	context: Arc<LedgerContext<DefaultDB>>,
	seed: WalletSeed,
	viewing_key: ViewingKeyFormat,
	/// Store processed transactions for LedgerContext synchronization
	processed_transactions: Vec<Transaction<Proof, DefaultDB>>,
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
	pub async fn sync_to_latest(&mut self) -> Result<(), WalletSyncError> {
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
}
