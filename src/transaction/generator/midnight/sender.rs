//!
//! Transaction sender utilities for the Midnight blockchain.
//!
//! Provides a sender struct and methods for submitting transactions to the Midnight network
//! using Subxt, including error handling and block finalization tracking.

use crate::transaction::generator::midnight::serialize;

use midnight_node_ledger_helpers::*;
use midnight_node_res::subxt_metadata::api as mn_meta;
use std::marker::PhantomData;
use subxt::{
    OnlineClient, PolkadotConfig,
    tx::{TxInBlock, TxProgress},
};
use tracing::{debug, error, info};

/// Transaction sender for submitting transactions to the Midnight network
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
    /// Creates a new transaction sender
    pub fn new(network_id: NetworkId, api: OnlineClient<PolkadotConfig>) -> Self {
        Self {
            network_id,
            api,
            _marker: PhantomData,
        }
    }

    /// Sends a transaction and waits for it to be included in a block
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

        let validation_result = unsigned_extrinsic.validate().await?;

        // Check if validation result indicates success
        match validation_result {
            subxt::tx::ValidationResult::Valid(_) => {
                // Transaction is valid, proceed with submission
                debug!("Transaction validated successfully");
            }
            subxt::tx::ValidationResult::Invalid(e) => {
                error!("Transaction validation failed: {:?}", e);
                return Err(subxt::Error::Other(format!(
                    "Transaction validation failed: {:?}",
                    e
                )));
            }
            subxt::tx::ValidationResult::Unknown(e) => {
                error!("Transaction validation unknown: {:?}", e);
                return Err(subxt::Error::Other(format!(
                    "Transaction validation unknown: {:?}",
                    e
                )));
            }
        }

        debug!("SENDING");
        let tx_progress = self
            .api
            .tx()
            .create_unsigned(&mn_tx)?
            .submit_and_watch()
            .await?;
        debug!("SENT");
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
            debug!("FAILED_TO_REACH_BEST_BLOCK");
            return;
        }
        let best_block = best_block.unwrap();
        let best_block_hash = best_block.block_hash();
        debug!("BEST_BLOCK - Block hash: {:?}", best_block_hash);

        // Check for ExtrinsicFailed events in the best block
        if let Err(e) = best_block.wait_for_success().await {
            error!("Transaction failed in best block: {:?}", e);
        } else {
            info!(
                "Transaction {:?} succeeded in best block: {:?}",
                tx_hash, best_block_hash
            );
        }

        let finalized = Self::wait_for_finalized(progress).await;
        match finalized {
            Some(finalized_block) => {
                let finalized_block_hash = finalized_block.block_hash();
                debug!("FINALIZED - Block hash: {:?}", finalized_block_hash);

                // Check for ExtrinsicFailed events in the finalized block as well
                if let Err(e) = finalized_block.wait_for_success().await {
                    error!("Transaction failed in finalized block: {:?}", e);
                } else {
                    info!(
                        "Transaction succeeded in finalized block: {:?}",
                        finalized_block_hash
                    );
                }
            }
            None => {
                error!("FAILED_TO_FINALIZE");
            }
        }
    }
}
