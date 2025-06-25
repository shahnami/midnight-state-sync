/// Transaction builder module for constructing Midnight transactions
pub mod builder;
/// Transaction generator utilities
pub mod generator {
	pub mod midnight;
}

/// Number of decimal places for the Midnight native token (tDUST).
pub const MIDNIGHT_TOKEN_DECIMALS: u32 = 6;
