//!
//! Utility functions for formatting token amounts for display.
//!
//! Provides helpers for converting raw token values to human-readable strings with decimal places.

/// Formats a token amount with the specified number of decimal places
pub fn format_token_amount(amount: u128, decimals: u32) -> String {
    format!(
        "{:.*}",
        decimals as usize,
        amount as f64 / 10f64.powi(decimals as i32)
    )
}
