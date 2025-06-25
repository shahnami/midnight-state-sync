//! Wallet Synchronization Module
//!
//! This module provides all the core logic and services for synchronizing a wallet with the Midnight blockchain.
//! It is composed of several submodules, each responsible for a specific aspect of the sync process:
//!
//! - `orchestrator`: The main entry point and coordinator for wallet sync. It wires together all services and strategies.
//! - `events`: Defines the event types and event handling traits used for decoupled communication between sync components.
//! - `merkle_update_service`: Handles parsing and applying Merkle tree updates to wallet state.
//! - `progress_tracker`: Tracks sync progress, processed indices, and provides statistics and validation.
//! - `strategies`: Contains pluggable sync strategies (relevant transactions) and their configuration.
//! - `transaction_processor`: Responsible for parsing and validating raw transaction data.
//!
//! The orchestrator coordinates the sync process by selecting a strategy, dispatching events, and invoking services for transaction and Merkle update processing. Progress tracking is integrated to ensure robust and observable synchronization.
//!
//! All submodules are designed to be modular and testable, with clear interfaces and responsibilities.

/// Event system for decoupled communication during sync
pub mod events;
/// Service for processing Merkle tree updates
pub mod merkle_update_service;
/// Main coordinator for the wallet sync process
pub mod orchestrator;
/// Tracks synchronization progress and statistics
pub mod progress_tracker;
/// Pluggable synchronization strategies
pub mod strategies;
/// Transaction parsing and validation service
pub mod transaction_processor;

pub use orchestrator::*;
