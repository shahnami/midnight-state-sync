pub mod address;
pub mod remote_prover;
pub mod sender;

use midnight_node_ledger_helpers::*;
use serde::{Deserialize, Serialize};

// Imported from pallet-midnight-rpc

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
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MidnightRpcTransaction {
	pub tx_hash: String,
	pub operations: Vec<Operation>,
	pub identifiers: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RpcTransaction {
	MidnightTransaction {
		#[serde(skip)]
		tx_raw: String,
		tx: MidnightRpcTransaction,
	},
	MalformedMidnightTransaction,
	Timestamp(u64),
	RuntimeUpgrade,
	UnknownTransaction,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RpcBlock<Header> {
	pub header: Header,
	pub body: Vec<RpcTransaction>,
	pub transactions_index: Vec<(String, String)>,
}
