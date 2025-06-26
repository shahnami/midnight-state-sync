//!
//! Midnight Wallet Sync
//!
//! This binary provides a command-line interface for synchronizing a Midnight wallet with the blockchain,
//! querying balances, and submitting transactions. It demonstrates how to use the modular wallet sync
//! orchestrator, indexer client, and transaction builder to interact with the Midnight network.
//!
//! Features:
//! - Modular wallet synchronization
//! - Transaction construction and submission
//! - Integration with remote proof servers
//! - Example of querying wallet balance and sending tDUST

mod indexer;
mod transaction;
mod utils;
mod wallet;

use midnight_node_ledger_helpers::{
    DefaultDB, InputInfo, LedgerContext, NATIVE_TOKEN, NetworkId, OfferInfo, OutputInfo, Proof,
    ProofProvider, TokenType, Transaction, Wallet, WalletKind, WalletSeed,
};
use rand::Rng;
use std::sync::Arc;
use subxt::{OnlineClient, PolkadotConfig};
use tracing::{debug, error, info};

use crate::{
    transaction::{
        builder::TransactionError,
        generator::midnight::{address::MidnightAddress, remote_prover::RemoteProofServer, sender},
    },
    utils::format_token_amount,
};

/// Main entry point for the Midnight wallet sync and transaction CLI.
///
/// This function initializes logging, sets up the indexer client, proof provider, and wallet context,
/// and runs the wallet sync orchestrator. It then queries the wallet balance and demonstrates how to
/// construct and submit a simple transfer transaction to the Midnight network.
///
/// # Panics
/// Panics if any required service fails to initialize or if transaction submission fails.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Initialize tracing subscriber with info logging for midnight crates
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("midnight_node_ledger_helpers=info".parse().unwrap())
                .add_directive("midnight_ledger_prototype=info".parse().unwrap())
                .add_directive("midnight_zswap=info".parse().unwrap())
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false)
        .with_timer(tracing_subscriber::fmt::time::time())
        .init();

    info!("Starting wallet sync service");
    let network = NetworkId::TestNet;

    let indexer_client = indexer::MidnightIndexerClient::new(
        "https://indexer.testnet-02.midnight.network/api/v1/graphql".to_string(),
        "wss://indexer.testnet-02.midnight.network/api/v1/graphql/ws".to_string(),
    );

    info!("Created indexer client");

    // Create proof provider
    let proof_provider = Box::new(RemoteProofServer::new(
        "http://localhost:6300".to_string(),
        network,
    ));

    info!("Created proof provider");

    let seed = "151f521307dab9fd5339aabc81542d30e2c33b0e0a1de5e80f3b343ccd72b7d4".to_string(); //wallet::generate_random_seed();
    let wallet_seed = WalletSeed::from(seed.as_str());
    let wallet = Wallet::<DefaultDB>::new(wallet_seed, 0, WalletKind::NoLegacy);
    let source_address = MidnightAddress::from_wallet(&wallet, network);

    debug!("Wallet seed: {:?}", seed);
    debug!("Wallet address: {:?}", source_address.encode());

    let destination_seed = wallet::generate_random_seed();
    let destination_wallet_seed = WalletSeed::from(destination_seed.as_str());

    // Requires
    // export MIDNIGHT_LEDGER_TEST_STATIC_DIR=user/path/to/midnightntwrk/midnight-node/static/contracts
    let context = Arc::new(LedgerContext::new_from_wallet_seeds(&[
        wallet_seed,
        // TODO: Figure out why we need to add destination wallet seed as well. It otherwise fails with:
        // thread 'main' panicked at /Users/nami/.cargo/git/checkouts/midnight-node-a5e2d7071ca76673/29935d2/ledger/helpers/src/context.rs:92:4:
        // Wallet with seed WalletSeed([...]) does not exists in the `LedgerContext
        destination_wallet_seed,
    ]));

    info!("Created context");

    // Use the new modular sync architecture
    let wallet_sync_orchestrator =
        wallet::WalletSyncOrchestrator::new(indexer_client, context.clone(), wallet_seed, network);

    let mut wallet_sync_orchestrator = match wallet_sync_orchestrator {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to start wallet sync service: {:?}", e);
            return;
        }
    };

    info!("Created wallet sync service");

    // The new orchestrator handles everything in a single sync() call
    // It will:
    // 1. Restore any saved state or checkpoints
    // 2. Sync from the appropriate starting point
    // 3. Apply all transactions and Merkle updates
    // 4. Save the final state
    wallet_sync_orchestrator
        .sync()
        .await
        .map_err(|e| {
            error!("Failed to sync wallet: {:?}", e);
        })
        .unwrap();

    info!("Wallet sync completed successfully");

    // Get current balance from wallet sync
    let available_utxo_value = wallet_sync_orchestrator.get_current_balance().await;
    info!(
        "Retrieved wallet balance: {} tDUST",
        format_token_amount(available_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS),
    );

    #[allow(clippy::identity_op)]
    let amount = 1 * 10u128.pow(transaction::MIDNIGHT_TOKEN_DECIMALS); // 1 tDUST

    let transaction = make_simple_transfer(
        available_utxo_value,
        context.clone(),
        wallet_seed,
        destination_wallet_seed,
        amount,
        NATIVE_TOKEN,
        proof_provider,
    )
    .await
    .unwrap();

    info!("Created transaction");

    info!("Sending transaction");

    let client = OnlineClient::<PolkadotConfig>::from_url("wss://rpc.testnet-02.midnight.network")
        .await
        .unwrap();

    let sender = sender::Sender::<Proof>::new(network, client.clone());

    match sender.send_tx(&transaction).await {
        Ok(()) => {
            info!("Transaction submitted successfully via Subxt");
            info!("{:#?}", transaction);
            // Do not apply transaction here - it should only be applied when confirmed on chain
        }
        Err(e) => {
            error!("Failed to submit transaction via Subxt: {}", e);
        }
    }
}

/// Creates a simple token transfer transaction from one wallet to another.
///
/// This function selects a UTXO, calculates fees, constructs the transaction offer, and builds a signed
/// transaction using the provided proof provider. It ensures that change is returned to the sender if
/// necessary and that the transaction is well-formed for the Midnight network.
///
/// # Arguments
/// * `available_utxo_value` - Total available balance in the source wallet
/// * `context` - Ledger context containing wallet information
/// * `from_wallet_seed` - Seed of the source wallet
/// * `to_wallet_seed` - Seed of the destination wallet
/// * `amount` - Amount to transfer (in smallest token units)
/// * `token_type` - Type of token to transfer
/// * `proof_provider` - Service for generating zero-knowledge proofs
///
/// # Returns
/// A signed transaction ready for submission to the network, or an error if construction fails.
#[allow(clippy::too_many_arguments)]
pub async fn make_simple_transfer(
    available_utxo_value: u128,
    context: Arc<LedgerContext<DefaultDB>>,
    from_wallet_seed: WalletSeed,
    to_wallet_seed: WalletSeed,
    amount: u128,
    token_type: TokenType,
    proof_provider: Box<dyn ProofProvider<DefaultDB>>,
) -> Result<Transaction<Proof, DefaultDB>, TransactionError> {
    info!("Starting simple transfer with wallet sync");
    // // Calculate transaction fees first to validate total requirement
    // let estimated_fee =
    // 	midnight_node_ledger_helpers::Wallet::<MidnightDefaultDB>::calculate_fee(1, 1);
    // let total_required = amount + estimated_fee;

    // Calculate fee based on transaction structure
    // For now, assume 2 outputs (recipient + change) in most cases
    // Note: The Wallet::calculate_fee method seems to overestimate fees significantly
    // Based on the error, actual fee for 1 input, 2 outputs is ~60,855 dust
    let actual_fee = 61000u128; // Conservative estimate for 1 input, 2 outputs (observed: 60,855)
    let total_required = amount + actual_fee;

    // Validate total amount including fees
    if total_required > available_utxo_value {
        return Err(TransactionError::InsufficientBalance(format!(
            "Requested {} tDUST + {} tDUST fee = {} tDUST total, but only {} tDUST available",
            format_token_amount(amount, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(total_required, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(available_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS),
        )));
    }
    info!(
        "Amount validated successfully (including {} tDUST fee)",
        format_token_amount(actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS)
    );

    // First, find the actual UTXO that will be spent to get its exact value
    let from_wallet = context.wallet_from_seed(from_wallet_seed);

    // Create a temporary input_info to find the minimum UTXO
    let temp_input_info = InputInfo::<WalletSeed> {
        origin: from_wallet_seed,
        token_type,
        value: amount + actual_fee, // Minimum amount needed including fees
    };

    // Find the actual UTXO that will be selected
    let selected_coin = temp_input_info.min_match_coin(&from_wallet.state);
    let actual_utxo_value = selected_coin.value;

    debug!("Selected UTXO details:");
    debug!(
        "   - Value: {} tDUST",
        format_token_amount(actual_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS)
    );
    debug!("   - Type: {:?}", selected_coin.type_);
    debug!("   - Nonce: {:?}", selected_coin.nonce);

    // Debug: Check the mt_index
    if let Some((_, qualified_coin_sp)) = from_wallet
        .state
        .coins
        .iter()
        .find(|(_, coin_sp)| coin_sp.nonce == selected_coin.nonce)
    {
        debug!("   - MT Index: {}", qualified_coin_sp.mt_index);
        debug!(
            "   - Wallet state first_free: {}",
            from_wallet.state.first_free
        );
    }

    // Verify the selected UTXO has sufficient value
    if actual_utxo_value < amount + actual_fee {
        error!(
            "Selected UTXO value {} tDUST is insufficient for amount {} tDUST + fee {} tDUST = {} tDUST",
            format_token_amount(actual_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(amount, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(amount + actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS)
        );
        return Err(TransactionError::InsufficientBalance(format!(
            "Selected UTXO value {} tDUST is insufficient for transaction amount {} tDUST + fee {} tDUST = {} tDUST",
            format_token_amount(actual_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(amount, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS),
            format_token_amount(amount + actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS)
        )));
    }

    debug!(
        "Found UTXO with value: {} tDUST (requested minimum: {} tDUST including {} tDUST fee)",
        format_token_amount(actual_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS),
        format_token_amount(amount + actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS),
        format_token_amount(actual_fee, transaction::MIDNIGHT_TOKEN_DECIMALS)
    );

    // Now create the actual input_info with the exact UTXO value that will be spent
    // This ensures the zero-knowledge proof references the correct UTXO
    let input_info = InputInfo::<WalletSeed> {
        origin: from_wallet_seed,
        token_type,
        value: actual_utxo_value, // Use the exact value of the selected UTXO
    };

    debug!(
        "Input info: {{origin: {:?}, token_type: {:?}, value: {} tDUST (actual UTXO value)}}",
        hex::encode(from_wallet_seed.0),
        token_type,
        format_token_amount(actual_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS),
    );

    // Create output to recipient
    let recipient_output = OutputInfo::<WalletSeed> {
        destination: to_wallet_seed,
        token_type,
        value: amount,
    };

    debug!(
        "Recipient output: {{destination: {:?}, token_type: {:?}, value: {} tDUST}}",
        hex::encode(to_wallet_seed.0),
        token_type,
        format_token_amount(amount, transaction::MIDNIGHT_TOKEN_DECIMALS)
    );

    // Create the guaranteed offer with input and outputs
    let mut offer = OfferInfo::default();
    offer.inputs.push(Box::new(input_info));
    offer.outputs.push(Box::new(recipient_output));

    // Calculate change and create change output if needed
    // This ensures the indexer recognizes the transaction as relevant to the sender
    let change_amount = actual_utxo_value.saturating_sub(amount + actual_fee);
    if change_amount > 0 {
        let change_output = OutputInfo::<WalletSeed> {
            destination: from_wallet_seed, // Send change back to sender
            token_type,
            value: change_amount,
        };
        offer.outputs.push(Box::new(change_output));
        debug!(
            "Change output: {{destination: self, token_type: {:?}, value: {} tDUST}}",
            token_type,
            format_token_amount(change_amount, transaction::MIDNIGHT_TOKEN_DECIMALS)
        );
    } else {
        debug!("No change output needed (exact amount)");
    }

    // Generate cryptographically secure random seed with timestamp for uniqueness
    let mut rng_seed = [0u8; 32];
    rand::rng().fill(&mut rng_seed);

    // Mix in current timestamp to ensure uniqueness across transaction attempts
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let timestamp_bytes = timestamp.to_le_bytes();

    // XOR the first 8 bytes of the seed with timestamp for uniqueness
    for (i, &byte) in timestamp_bytes.iter().enumerate() {
        rng_seed[i] ^= byte;
    }

    // Build and return the transaction
    transaction::builder::MidnightTransactionBuilder::<DefaultDB>::new()
        .with_context(context)
        .with_proof_provider(proof_provider)
        .with_rng_seed(rng_seed)
        .with_guaranteed_offer(offer)
        .build()
        .await
}
