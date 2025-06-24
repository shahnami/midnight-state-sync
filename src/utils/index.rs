pub fn format_token_amount(amount: u128, decimals: u32) -> String {
    format!(
        "{:.*}",
        decimals as usize,
        amount as f64 / 10f64.powi(decimals as i32)
    )
}
