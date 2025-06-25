//!
//! Wallet module for Midnight blockchain.
//!
//! Provides wallet types, synchronization logic, and error handling for interacting with the
//! Midnight blockchain. Includes utilities for generating random wallet seeds.
/// Wallet synchronization functionality
pub mod sync;
/// Type definitions for wallet operations
pub mod types;

pub use sync::*;
pub use types::*;

use rand::Rng;

/// Generates a random 32-byte seed encoded as a hex string.
///
/// # Returns
/// A 64-character hexadecimal string representing a random 32-byte seed.
pub fn generate_random_seed() -> String {
	let mut seed = [0u8; 32];
	rand::rng().fill(&mut seed);
	hex::encode(seed)
}
