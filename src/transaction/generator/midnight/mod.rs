//!
//! Midnight transaction generator module.
//!
//! Provides utilities for encoding/decoding addresses, integrating with remote proof servers,
//! and sending transactions on the Midnight blockchain. Includes types for representing
//! operations, transactions, and blocks as used in RPC and indexer responses.
/// Address encoding/decoding utilities for Midnight
pub mod address;
/// Remote proof server integration
pub mod remote_prover;
/// Transaction sender utilities
pub mod sender;

use midnight_node_ledger_helpers::*;
use serde::{Deserialize, Serialize};

// Imported from pallet-midnight-rpc

/// Operations that can be performed in a Midnight transaction.
///
/// This enum represents the different types of operations that can be included in a Midnight transaction.
///
/// Variants:
/// - `Call`: Calls a contract at a given address and entry point.
/// - `Deploy`: Deploys a contract at a given address.
/// - `FallibleCoins`: Represents a fallible coin operation.
/// - `GuaranteedCoins`: Represents a guaranteed coin operation.
/// - `Maintain`: Performs a maintenance operation on a contract at a given address.
/// - `ClaimMint`: Claims minted coins of a specific type and value.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Operation {
    Call {
        address: String,
        entry_point: String,
    },
    Deploy {
        address: String,
    },
    FallibleCoins,
    GuaranteedCoins,
    Maintain {
        address: String,
    },
    ClaimMint {
        value: u128,
        coin_type: String,
    },
}
/// RPC representation of a Midnight transaction.
///
/// Contains the transaction hash, a list of operations, and associated identifiers.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MidnightRpcTransaction {
    /// The transaction hash.
    pub tx_hash: String,
    /// The list of operations included in the transaction.
    pub operations: Vec<Operation>,
    /// Identifiers associated with the transaction.
    pub identifiers: Vec<String>,
}

/// Different types of transactions that can appear in RPC responses.
///
/// This enum represents the possible transaction types returned by the RPC, including valid Midnight transactions,
/// malformed transactions, timestamps, runtime upgrades, and unknown transactions.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RpcTransaction {
    /// A valid Midnight transaction with raw data and parsed fields.
    MidnightTransaction {
        #[serde(skip)]
        tx_raw: String,
        tx: MidnightRpcTransaction,
    },
    /// A malformed Midnight transaction.
    MalformedMidnightTransaction,
    /// A timestamp entry.
    Timestamp(u64),
    /// A runtime upgrade event.
    RuntimeUpgrade,
    /// An unknown transaction type.
    UnknownTransaction,
}

/// RPC representation of a block containing transactions.
///
/// Contains the block header, a list of transactions, and a transaction index mapping.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RpcBlock<Header> {
    /// The block header.
    pub header: Header,
    /// The list of transactions in the block.
    pub body: Vec<RpcTransaction>,
    /// A mapping of transaction hashes to additional data.
    pub transactions_index: Vec<(String, String)>,
}
