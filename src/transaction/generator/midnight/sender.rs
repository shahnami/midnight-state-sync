use midnight_node_ledger_helpers::*;
use midnight_node_res::subxt_metadata::api as mn_meta;
use std::marker::PhantomData;
use subxt::{
	OnlineClient, PolkadotConfig,
	tx::{TxInBlock, TxProgress},
};

// Import error types for proper error handling
use subxt::error::DispatchError;

use crate::transaction::generator::midnight::serialize;

pub struct Sender<P: Proofish<DefaultDB> + Send + Sync + 'static> {
	network_id: NetworkId,
	api: OnlineClient<PolkadotConfig>,
	_marker: PhantomData<P>,
}

impl<P: Proofish<DefaultDB> + Send + Sync + 'static> Sender<P>
where
	<P as Proofish<DefaultDB>>::Pedersen: Send + Sync,
	<P as Proofish<DefaultDB>>::LatestProof: Send + Sync,
	<P as Proofish<DefaultDB>>::Proof: Send + Sync,
{
	pub fn new(network_id: NetworkId, api: OnlineClient<PolkadotConfig>) -> Self {
		Self {
			network_id,
			api,
			_marker: PhantomData,
		}
	}

	pub async fn send_tx(&self, tx: &Transaction<P, DefaultDB>) -> Result<(), subxt::Error> {
		let (tx_hash_string, tx_progress) = self.send_tx_no_wait(tx).await?;
		Self::send_and_log(&tx_hash_string, tx_progress).await;
		Ok(())
	}

	async fn send_tx_no_wait(
		&self,
		tx: &Transaction<P, DefaultDB>,
	) -> Result<
		(
			String,
			TxProgress<PolkadotConfig, OnlineClient<PolkadotConfig>>,
		),
		subxt::Error,
	> {
		let mn_tx = mn_meta::tx()
			.midnight()
			.send_mn_transaction(hex::encode(serialize(tx, self.network_id).unwrap()).into_bytes());
		let unsigned_extrinsic = self.api.tx().create_unsigned(&mn_tx)?;
		let tx_hash_string = format!("0x{}", hex::encode(unsigned_extrinsic.hash().as_bytes()));

		log::info!("SENDING");
		let tx_progress = self
			.api
			.tx()
			.create_unsigned(&mn_tx)?
			.submit_and_watch()
			.await?;
		log::info!("SENT");
		Ok((tx_hash_string, tx_progress))
	}

	async fn wait_for_best_block(
		mut progress: TxProgress<PolkadotConfig, OnlineClient<PolkadotConfig>>,
	) -> (
		TxProgress<PolkadotConfig, OnlineClient<PolkadotConfig>>,
		Option<TxInBlock<PolkadotConfig, OnlineClient<PolkadotConfig>>>,
	) {
		while let Some(prog) = progress.next().await {
			if let Ok(subxt::tx::TxStatus::InBestBlock(info)) = prog {
				return (progress, Some(info));
			}
		}

		(progress, None)
	}

	async fn wait_for_finalized(
		mut progress: TxProgress<PolkadotConfig, OnlineClient<PolkadotConfig>>,
	) -> Option<TxInBlock<PolkadotConfig, OnlineClient<PolkadotConfig>>> {
		while let Some(prog) = progress.next().await {
			if let Ok(subxt::tx::TxStatus::InFinalizedBlock(info)) = prog {
				return Some(info);
			}
		}

		None
	}

	async fn send_and_log(
		tx_hash: &str,
		tx: TxProgress<PolkadotConfig, OnlineClient<PolkadotConfig>>,
	) {
		let (progress, best_block) = Self::wait_for_best_block(tx).await;
		if best_block.is_none() {
			log::info!("FAILED_TO_REACH_BEST_BLOCK");
			return;
		}
		let best_block = best_block.unwrap();
		let best_block_hash = best_block.block_hash();
		log::info!("BEST_BLOCK - Block hash: {:?}", best_block_hash);

		// Check for ExtrinsicFailed events in the best block
		if let Err(e) = best_block.wait_for_success().await {
			log::error!("❌ Transaction failed in best block: {:?}", e);

			// Try to extract more detailed error information
			match e {
				subxt::Error::Runtime(runtime_error) => {
					log::error!("❌ Runtime error details: {:?}", runtime_error);

					// Extract specific error details for Midnight::Transaction errors
					match &runtime_error {
						DispatchError::Module(module_error) => {
							log::error!("❌ Module error details: {:?}", module_error);

							// Access raw error bytes to decode nested error
							let error_bytes = module_error.bytes();
							log::error!("❌ Raw error bytes: {:02x?}", error_bytes);
							log::error!(
								"❌ Pallet index: {}, Error index: {}",
								error_bytes[0],
								error_bytes[1]
							);

							// Decode the specific nested error for Midnight::Transaction
							if error_bytes[0] == 5 && error_bytes[1] == 3 {
								// Midnight pallet (5), Transaction error (3)
								log::error!("Decoding Midnight::Transaction nested error...");

								// bytes[2] should contain the TransactionError variant index
								let transaction_error_index = error_bytes[2];
								log::error!(
									"TransactionError variant index: {}",
									transaction_error_index
								);

								match transaction_error_index {
									0 => {
										log::error!("Specific error: Invalid(InvalidError)");
										// bytes[3] contains the InvalidError variant
										let invalid_error_index = error_bytes[3];
										log::error!(
											"InvalidError variant index: {}",
											invalid_error_index
										);

										match invalid_error_index {
											0 => log::error!("InvalidError: EffectsMismatch"),
											1 => {
												log::error!("InvalidError: ContractAlreadyDeployed")
											}
											2 => log::error!("InvalidError: ContractNotPresent"),
											3 => log::error!("InvalidError: Zswap"),
											4 => log::error!("InvalidError: Transcript"),
											5 => log::error!("InvalidError: InsufficientClaimable"),
											6 => {
												log::error!("InvalidError: VerifierKeyNotFound")
											}
											7 => log::error!(
												"InvalidError: VerifierKeyAlreadyPresent"
											),
											8 => log::error!("InvalidError: ReplayCounterMismatch"),
											9 => log::error!("InvalidError: UnknownError"),
											_ => log::error!(
												"InvalidError: Unknown variant {}",
												invalid_error_index
											),
										}
									}
									1 => {
										log::error!("Specific error: Malformed(MalformedError)");
										log::error!(
											"MalformedError variant index: {}",
											error_bytes[3]
										);
									}
									2 => {
										log::error!(
											"Specific error: SystemTransaction(SystemTransactionError)"
										);
										log::error!(
											"SystemTransactionError variant index: {}",
											error_bytes[3]
										);
									}
									_ => {
										log::error!(
											"Unknown TransactionError variant: {}",
											transaction_error_index
										);
									}
								}
							}

							// Also try to get metadata details if available
							if let Ok(details) = module_error.details() {
								log::error!(
									"❌ Metadata - pallet: {}, variant: {}, error: {:?}",
									details.pallet.name(),
									details.variant.name,
									details.variant.index
								);
							}
						}
						_ => {
							log::error!("❌ Non-module runtime error: {:?}", runtime_error);
						}
					}
				}
				_ => {
					log::error!("❌ Non-runtime error: {:?}", e);
				}
			}
		} else {
			log::info!("✅ Transaction succeeded in best block");
		}

		let finalized = Self::wait_for_finalized(progress).await;
		match finalized {
			Some(finalized_block) => {
				let finalized_block_hash = finalized_block.block_hash();
				log::info!("FINALIZED - Block hash: {:?}", finalized_block_hash);

				// Check for ExtrinsicFailed events in the finalized block as well
				if let Err(e) = finalized_block.wait_for_success().await {
					log::error!("❌ Transaction failed in finalized block: {:?}", e);

					// Extract detailed error information for finalized block as well
					match e {
						subxt::Error::Runtime(runtime_error) => match &runtime_error {
							DispatchError::Module(module_error) => {
								log::error!(
									"❌ Finalized block - Module error details: {:?}",
									module_error
								);

								if let Ok(details) = module_error.details() {
									log::error!(
										"❌ Finalized block - pallet: {}, variant: {}, error: {:?}",
										details.pallet.name(),
										details.variant.name,
										details.variant.index
									);
								}
							}
							_ => {
								log::error!(
									"❌ Finalized block - Non-module runtime error: {:?}",
									runtime_error
								);
							}
						},
						_ => {
							log::error!("❌ Finalized block - Non-runtime error: {:?}", e);
						}
					}
				} else {
					log::info!("✅ Transaction succeeded in finalized block");
				}
			}
			None => {
				log::info!("FAILED_TO_FINALIZE");
			}
		}
		log::info!("TRANSACTION HASH: {}", tx_hash);
	}
}
