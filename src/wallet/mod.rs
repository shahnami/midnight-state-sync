pub mod sync;
pub mod types;

pub use sync::MidnightWalletSyncService;
pub use types::*;

use rand::Rng;

pub fn generate_random_seed() -> String {
	let mut seed = [0u8; 32];
	rand::rng().fill(&mut seed);
	hex::encode(seed)
}
