mod indexer;
mod transaction;
mod utils;
mod wallet;

use midnight_node_ledger_helpers::{
	DefaultDB, InputInfo, LedgerContext, NATIVE_TOKEN, NetworkId, OfferInfo, OutputInfo, Proof,
	ProofProvider, TokenType, Transaction, WalletSeed,
};
use rand::Rng;
use std::sync::Arc;
use subxt::{OnlineClient, PolkadotConfig};
use tracing::{error, info};

use crate::transaction::{
	builder::TransactionError,
	generator::midnight::{remote_prover::RemoteProofServer, sender},
};
use crate::utils::format_token_amount;

#[tokio::main(flavor = "current_thread")]
async fn main() {
	// Initialize tracing subscriber with debug logging for midnight crates
	tracing_subscriber::fmt()
		.with_env_filter(
			tracing_subscriber::EnvFilter::from_default_env()
				.add_directive("midnight_node_ledger_helpers=debug".parse().unwrap())
				.add_directive("midnight_ledger_prototype=debug".parse().unwrap())
				.add_directive("midnight_zswap=debug".parse().unwrap())
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

	let client = OnlineClient::<PolkadotConfig>::from_url("wss://rpc.testnet-02.midnight.network")
		.await
		.unwrap();

	let sender = sender::Sender::<Proof>::new(network, client);

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

	let seed = "2e347e236daa04faad881f1dc5dc3b8a9b4e8e4429e9d0728aad78ada199b66b".to_string(); //wallet::generate_random_seed();
	let wallet_seed = WalletSeed::from(seed.as_str());

	let destination_seed = wallet::generate_random_seed();
	let destination_wallet_seed = WalletSeed::from(destination_seed.as_str());

	// Requires
	// export MIDNIGHT_LEDGER_TEST_STATIC_DIR=user/path/to/midnightntwrk/midnight-node/static/contracts
	let context = Arc::new(LedgerContext::new_from_wallet_seeds(&[
		wallet_seed,
		// TODO: Figure out why we need to add destination wallet seed as well. It otherwise fails with wallet with seed does not exist in LedgerContext.
		destination_wallet_seed,
	]));

	info!("Created context");

	let wallet_sync_service = wallet::MidnightWalletSyncService::new(
		indexer_client,
		context.clone(),
		wallet_seed,
		network,
	)
	.await;

	let mut wallet_sync_service = match wallet_sync_service {
		Ok(service) => service,
		Err(e) => {
			error!("Failed to start wallet sync service: {:?}", e);
			return;
		}
	};

	info!("Created wallet sync service");

	// Syncs the latest updates and stores in service
	wallet_sync_service
		.sync_to_latest()
		.await
		.map_err(|e| {
			error!("Failed to sync wallet: {:?}", e);
		})
		.unwrap();

	wallet_sync_service
		.apply_collapsed_updates()
		.await
		.map_err(|e| {
			error!("Failed to apply collapsed updates: {:?}", e);
		})
		.unwrap();

	wallet_sync_service
		.apply_transactions()
		.await
		.map_err(|e| {
			error!("Failed to apply transactions: {:?}", e);
		})
		.unwrap();

	// Get current balance from wallet sync
	let available_utxo_value = wallet_sync_service.get_current_balance().await;
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

	match sender.send_tx(&transaction).await {
		Ok(()) => {
			info!("Transaction submitted successfully via Subxt");
			info!("{:#?}", transaction);
		}
		Err(e) => {
			error!("Failed to submit transaction via Subxt: {}", e);
		}
	}
}

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

	// Use actual observed fee from well_formed validation (instead of inflated estimate)
	// The Wallet::calculate_fee(1, 1) method returns 444M dust which is incorrect
	// The actual fee for simple transfers is around 40K dust based on well_formed logs
	let actual_fee = 50000u128; // Conservative estimate based on observed 40,391 dust
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

	info!("Selected UTXO details:");
	info!(
		"   - Value: {} tDUST",
		format_token_amount(actual_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS)
	);
	info!("   - Type: {:?}", selected_coin.type_);
	info!("   - Nonce: {:?}", selected_coin.nonce);

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

	info!(
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

	info!(
		"Input info: {{origin: {:?}, token_type: {:?}, value: {} tDUST (actual UTXO value)}}",
		from_wallet_seed,
		token_type,
		format_token_amount(actual_utxo_value, transaction::MIDNIGHT_TOKEN_DECIMALS),
	);

	// Create output to recipient
	let recipient_output = OutputInfo::<WalletSeed> {
		destination: to_wallet_seed,
		token_type,
		value: amount,
	};

	info!(
		"Recipient output: {{destination: {:?}, token_type: {:?}, value: {} tDUST}}",
		to_wallet_seed,
		token_type,
		format_token_amount(amount, transaction::MIDNIGHT_TOKEN_DECIMALS)
	);

	// Create the guaranteed offer with input and recipient output
	// The midnight-node library automatically handles:
	// 1. Fee calculation and deduction
	// 2. Change output creation (shown in deltas)
	// 3. Balance validation during well_formed check
	let mut offer = OfferInfo::default();
	offer.inputs.push(Box::new(input_info));
	offer.outputs.push(Box::new(recipient_output));

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
