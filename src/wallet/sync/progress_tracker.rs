//! Progress tracking for wallet synchronization.
//!
//! This module provides the `SyncProgressTracker`, which is responsible for tracking the progress
//! of the wallet sync process. It records processed indices, counts transactions and Merkle updates,
//! and provides statistics and validation to ensure sync completeness.
//!
//! The progress tracker is used by sync strategies and the orchestrator to monitor and log sync
//! progress, detect gaps, and validate completion.

use std::collections::HashSet;
use tracing::{info, warn};

/// Service for tracking synchronization progress
///
/// The progress tracker records which blockchain indices have been processed, counts transactions
/// and Merkle updates, and provides statistics and validation methods for sync completeness.
#[derive(Debug, Clone)]
pub struct SyncProgressTracker {
    /// The highest blockchain index we've processed
    highest_processed_index: u64,
    /// Track if we've seen any data events
    has_processed_data: bool,
    /// Track all blockchain indices we've processed
    processed_indices: HashSet<u64>,
    /// Starting index for this sync session
    start_index: u64,
    /// Total transactions processed
    transactions_processed: usize,
    /// Total Merkle updates processed
    merkle_updates_processed: usize,
    /// Last index at which we logged progress
    last_logged_index: u64,
}

impl SyncProgressTracker {
    /// Create a new progress tracker starting from the given index.
    pub fn new(start_index: u64) -> Self {
        Self {
            highest_processed_index: start_index,
            has_processed_data: false,
            processed_indices: HashSet::new(),
            start_index,
            transactions_processed: 0,
            merkle_updates_processed: 0,
            last_logged_index: start_index,
        }
    }

    /// Record that we processed data at a specific index
    pub fn record_processed(&mut self, index: u64) {
        self.highest_processed_index = self.highest_processed_index.max(index);
        self.has_processed_data = true;
        self.processed_indices.insert(index);
    }

    /// Record a processed transaction at the given index
    pub fn record_transaction(&mut self, index: u64) {
        self.record_processed(index);
        self.transactions_processed += 1;
    }

    /// Record a processed Merkle update at the given index
    pub fn record_merkle_update(&mut self, index: u64) {
        self.record_processed(index);
        self.merkle_updates_processed += 1;
    }

    /// Check if sync is complete based on progress updates
    ///
    /// Returns true if all relevant indices have been processed and at least some data was processed.
    pub fn is_sync_complete(&self, highest_index: u64, highest_relevant_wallet_index: u64) -> bool {
        // Only consider sync complete if:
        // 1. We've reached the highest relevant index
        // 2. We've actually processed data up to that point
        // 3. We've processed at least some data (not just metadata)
        highest_index >= highest_relevant_wallet_index
            && self.highest_processed_index >= highest_relevant_wallet_index
            && self.has_processed_data
    }

    /// Check for gaps in processed indices
    ///
    /// Returns a list of (start, end) pairs for missing index ranges.
    pub fn check_for_gaps(&self) -> Vec<(u64, u64)> {
        let mut gaps = Vec::new();

        if self.processed_indices.len() <= 1 {
            return gaps;
        }

        let mut sorted_indices: Vec<u64> = self.processed_indices.iter().cloned().collect();
        sorted_indices.sort();

        for window in sorted_indices.windows(2) {
            if window[1] - window[0] > 1 {
                gaps.push((window[0], window[1]));
            }
        }

        gaps
    }

    /// Log progress at regular intervals or when forced
    pub fn log_progress(&mut self, force: bool) {
        // Log every 1000 blocks/indices or when forced
        let blocks_since_last_log = self
            .highest_processed_index
            .saturating_sub(self.last_logged_index);
        let should_log = force || blocks_since_last_log >= 1000;

        if should_log && self.has_processed_data {
            info!(
                "Sync progress: {} transactions, {} Merkle updates processed up to index {}",
                self.transactions_processed,
                self.merkle_updates_processed,
                self.highest_processed_index
            );
            self.last_logged_index = self.highest_processed_index;
        }
    }

    /// Get sync statistics as a SyncStats struct
    pub fn get_stats(&self) -> SyncStats {
        SyncStats {
            start_index: self.start_index,
            highest_processed_index: self.highest_processed_index,
            has_processed_data: self.has_processed_data,
            total_indices_processed: self.processed_indices.len(),
            transactions_processed: self.transactions_processed,
            merkle_updates_processed: self.merkle_updates_processed,
            gaps: self.check_for_gaps(),
        }
    }

    /// Validate sync completion, returning an error if incomplete or gaps are found
    pub fn validate_completion(&self) -> Result<(), String> {
        if !self.has_processed_data {
            return Err("Sync completed without processing any data".to_string());
        }

        let gaps = self.check_for_gaps();
        if !gaps.is_empty() {
            warn!(
                "Sync completed with {} gaps in processed indices",
                gaps.len()
            );
            for (start, end) in &gaps {
                warn!(
                    "Gap detected: missing indices between {} and {}",
                    start, end
                );
            }
        }

        Ok(())
    }
}

/// Statistics about the sync progress
///
/// This struct summarizes the progress of the sync session, including processed indices,
/// transaction and Merkle update counts, and any detected gaps.
#[derive(Debug, Clone)]
pub struct SyncStats {
    pub start_index: u64,
    pub highest_processed_index: u64,
    pub has_processed_data: bool,
    pub total_indices_processed: usize,
    pub transactions_processed: usize,
    pub merkle_updates_processed: usize,
    pub gaps: Vec<(u64, u64)>,
}

impl SyncStats {
    /// Get a human-readable summary of the sync statistics
    pub fn summary(&self) -> String {
        format!(
            "Sync from {} to {}: {} transactions, {} Merkle updates, {} total indices{}",
            self.start_index,
            self.highest_processed_index,
            self.transactions_processed,
            self.merkle_updates_processed,
            self.total_indices_processed,
            if self.gaps.is_empty() {
                String::new()
            } else {
                format!(" ({} gaps)", self.gaps.len())
            }
        )
    }
}
