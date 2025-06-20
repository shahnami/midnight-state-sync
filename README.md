# Midnight State Sync Demo

This repository demonstrates a critical bug in the Midnight blockchain wallet implementation where incomplete synchronization permanently corrupts the wallet state, preventing all future transactions from succeeding.

## Bug Summary

When the wallet sync completes prematurely (before receiving all merkle tree updates), any transaction submitted with the incomplete merkle root will:
1. Fail on-chain with `InvalidError: Zswap`
2. Corrupt the wallet's internal state
3. Prevent ALL future transactions from succeeding, even after complete resyncs

This effectively "bricks" the wallet until manual state reset.

## Prerequisites

- Rust and Cargo installed
- Access to Midnight testnet
- Local proof server running on port 6300

## Setup

1. Clone the repository:

```bash
git clone https://github.com/shahnami/midnight-state-sync.git
cd midnight-state-sync
```

2. Set the required environment variable for Midnight static contracts:

```bash
export MIDNIGHT_LEDGER_TEST_STATIC_DIR=/PATH/TO/midnightntwrk/midnight-node/static/contracts
```

3. Start the local proof server (required for transaction proving):

```bash
# In a separate terminal, start your Midnight proof server on port 6300
docker run -p 6300:6300 midnightnetwork/proof-server -- 'midnight-proof-server --network testnet'
```

## Running the Demo

Run the demo with:

```bash
cargo run
```

## Bug Details

### The Problem

The wallet sync service has a race condition where it considers synchronization complete based on `ProgressUpdate` events rather than confirming all data has been received. This leads to:

1. **Incomplete Sync**: The sync completes with only partial merkle tree state
2. **Failed Transaction**: Transactions fail with "unknown Merkle tree" errors
3. **State Corruption**: The wallet optimistically updates its state for the failed transaction
4. **Permanent Failure**: All subsequent transactions fail, even after "complete" syncs

### Evidence from Logs

From `tx11.log` (incomplete sync):
```
Processing event: ProgressUpdate {
    highest_index: 10555,
    highest_relevant_index: 10555,
    highest_relevant_wallet_index: 10555,
}
Wallet sync completed
Sync completed! Processed 4 events
```

Later in the same transaction:
```
attempted spend with unknown Merkle tree input.merkle_tree_root=76833aa705dc4ba23abda8f755258c8898cea751a36367ae22164707204fa522
Transaction failed in finalized block: Runtime(Module(ModuleError(<Midnight::Transaction>)))
InvalidError: Zswap
```

From `tx12.log` (after tx11 failed):
```
Sync completed! Processed 12 events  // More complete sync than tx11
...
attempted spend with unknown Merkle tree input.merkle_tree_root=213db2382c118edf56f65adef25eaf269279e986de40e1aafc76173c5a534339
Transaction failed in finalized block: Runtime(Module(ModuleError(<Midnight::Transaction>)))
InvalidError: Zswap
```

### Root Causes

1. **Race Condition in Sync**: The indexer reports completion via `ProgressUpdate` before ensuring all events are delivered
2. **No Transaction Rollback**: Failed transactions aren't rolled back in the wallet state
3. **No State Validation**: The wallet doesn't verify merkle tree consistency before transactions
4. **No Recovery Mechanism**: Once corrupted, the wallet state cannot self-repair

### Impact

- One incomplete sync permanently breaks the wallet
- Users must manually reset wallet state to recover
- No indication to users that the wallet is in a corrupted state
- Subsequent syncs appear to complete but don't fix the corruption

## Technical Analysis

The sync completion logic in `wallet/sync.rs`:
```rust
if highest_index >= highest_relevant_wallet_index {
    info!("Wallet sync completed");
    break;
}
```

This relies on metadata rather than confirming data receipt, creating a window where sync can complete without all merkle tree updates.

## Workaround

Currently, the only workaround is to:
1. Detect transaction failures due to "unknown Merkle tree"
2. Completely reset wallet state
3. Perform a full resync from genesis
4. Retry the transaction

## Key Files

- `src/main.rs` - Main entry point that orchestrates the wallet sync and transaction
- `src/wallet/sync.rs` - Wallet synchronization service (contains the race condition)
- `src/transaction/` - Transaction building and sending logic
- `src/indexer/` - GraphQL client for the Midnight indexer

## Configuration

The demo uses:

- **Network**: Midnight Testnet-02
- **RPC**: `wss://rpc.testnet-02.midnight.network`
- **Indexer**: `https://indexer.testnet-02.midnight.network/api/v1/graphql`
- **Proof Server**: `http://localhost:6300`

## Reproduction Steps

1. Fund a fresh wallet from the faucet
2. Run multiple transactions in succession
3. Eventually, a sync will complete prematurely
4. That transaction and all subsequent transactions will fail
5. Even after restarting and resyncing, transactions continue to fail

## Logs

Example logs demonstrating the issue are included:
- `tx10.log` - Successful transaction (last working state)
- `tx11.log` - Incomplete sync leading to failed transaction
- `tx12.log` - Subsequent transaction failing despite "complete" sync

These logs show how the wallet state becomes permanently corrupted after a single incomplete sync.