# Midnight State Sync Service

A proof-of-concept (PoC) implementation of a robust wallet synchronization service for the Midnight blockchain. This service demonstrates advanced state management, chronological transaction ordering, and proper handling of failed transactions to maintain wallet integrity.

## Overview

The Midnight State Sync Service provides a reliable way to synchronize wallet state with the blockchain, featuring:

- **Chronological Update Ordering**: Ensures merkle tree updates and transactions are applied in the correct blockchain order
- **Transaction Status Filtering**: Only applies successful transactions to wallet state
- **Event-Driven Architecture**: Modular design with clear separation of concerns
- **Viewing Key-Based Sync**: Efficient synchronization using wallet viewing keys
- **Comprehensive Error Handling**: Proper handling of failed transactions and network issues

## Architecture

```mermaid
graph TB
    subgraph "External Services"
        RPC[Midnight RPC Node]
        Indexer[GraphQL Indexer]
        ProofServer[Proof Server]
    end

    subgraph "Sync Orchestrator"
        Orchestrator[WalletSyncOrchestrator]
        EventDispatcher[Event Dispatcher]
        EventHandler[Event Handler]
    end

    subgraph "Sync Strategy"
        RelevantSync[RelevantTransactionSync]
    end

    subgraph "Processing Services"
        TxProcessor[TransactionProcessor]
        MerkleService[MerkleTreeUpdateService]
    end

    subgraph "Storage"
        UpdateBuffer[(Chronological Updates Buffer)]
        WalletState[(Wallet State)]
        LedgerState[(Ledger State)]
    end

    %% Connections
    Indexer --> RelevantSync

    RelevantSync --> EventDispatcher

    EventDispatcher --> EventHandler
    EventHandler --> TxProcessor
    EventHandler --> MerkleService
    EventHandler --> UpdateBuffer

    Orchestrator --> UpdateBuffer
    Orchestrator --> WalletState
    Orchestrator --> LedgerState

    ProofServer --> Orchestrator
    RPC --> Orchestrator
```

## Application Flow Diagram

This sequence diagram shows the detailed flow from entrypoint through the orchestrator's behavior:

```mermaid
sequenceDiagram
    participant Main
    participant Orchestrator
    participant Strategy
    participant EventDispatcher
    participant EventHandler
    participant Services
    participant Buffer
    participant State

    Main->>Main: Initialize Config
    Main->>Main: Create LedgerContext
    Main->>Main: Create IndexerClient
    
    Main->>Orchestrator: new(config)
    activate Orchestrator
    
    Note over Orchestrator: Initialization Phase
    Orchestrator->>Orchestrator: Derive Viewing Key
    Orchestrator->>Services: Create TransactionProcessor
    Orchestrator->>Services: Create MerkleService
    Orchestrator->>Strategy: Create RelevantTransactionSync
    
    Orchestrator-->>Main: Return Orchestrator
    deactivate Orchestrator
    
    Main->>Orchestrator: sync()
    activate Orchestrator
    
    Note over Orchestrator: Sync Phase
    
    
    Orchestrator->>Buffer: Create Update Buffer
    Orchestrator->>EventHandler: Create with Services
    Orchestrator->>EventDispatcher: Register Handler
    
    Orchestrator->>Strategy: sync(start_height)
    activate Strategy
    
    loop Sync Events
        Strategy->>Strategy: Fetch from Indexer
        Strategy->>EventDispatcher: dispatch(event)
        EventDispatcher->>EventHandler: handle(event)
        
        alt TransactionReceived
            EventHandler->>Services: process_transaction()
            EventHandler->>Buffer: Push Transaction Update
        else MerkleUpdateReceived
            EventHandler->>Services: process_merkle_update()
            EventHandler->>Buffer: Push Merkle Update
        end
    end
    
    Strategy->>EventDispatcher: dispatch(SyncCompleted)
    Strategy-->>Orchestrator: Done
    deactivate Strategy
    
    Note over Orchestrator: Update Application Phase
    
    Orchestrator->>Buffer: Get All Updates
    Orchestrator->>Orchestrator: Sort by Blockchain Index
    
    loop Apply Updates
        alt Update is MerkleUpdate
            Orchestrator->>Services: apply_collapsed_update()
            Services->>State: Update Merkle Tree
        else Update is Transaction
            Orchestrator->>Orchestrator: Check ApplyStage
            alt SucceededEntirely or SucceededPartially
                Orchestrator->>State: update_from_txs()
            else Failed or Pending
                Orchestrator->>Orchestrator: Skip Transaction
            end
        end
    end
    
    
    Orchestrator-->>Main: Success
    deactivate Orchestrator
    
    Main->>Orchestrator: get_current_balance()
    Orchestrator->>State: Read Wallet Coins
    Orchestrator-->>Main: Return Balance
```

## Component Flow

### 1. Sync Initialization

```
Main → WalletSyncOrchestrator → Select Strategy → Initialize Services
```

### 2. Event Processing Flow

```
Indexer → Strategy → Event Conversion → Event Dispatcher → Event Handler → Buffer Updates
```

### 3. Update Application

```
Buffer → Sort by Index → Apply Merkle Updates → Filter & Apply Transactions → Update State
```

## Key Components

### Sync Orchestrator (`orchestrator.rs`)

The main coordinator that:

- Initializes all services
- Manages the sync lifecycle
- Buffers updates chronologically
- Applies updates in correct order

### Sync Strategy

#### RelevantTransactionSync

- Uses wallet viewing key for efficient sync
- Only fetches transactions relevant to the wallet
- Ideal for normal wallet operations

### Event System

#### Event Types

- `TransactionReceived`: New transaction to process
- `MerkleUpdateReceived`: Merkle tree update
- `ProgressUpdate`: Sync progress information
- `SyncCompleted`: Sync finished successfully
- `SyncError`: Error during synchronization

### Processing Services

#### TransactionProcessor

- Parses raw transaction data
- Validates transaction format
- Filters based on `ApplyStage`

#### MerkleTreeUpdateService

- Processes merkle tree updates
- Applies updates to wallet state
- Maintains merkle tree consistency


## Configuration

### Environment Variables

```bash
# Required: Path to Midnight static contracts
export MIDNIGHT_LEDGER_TEST_STATIC_DIR=/path/to/midnight-node/static/contracts
```

### Creating the Orchestrator

```rust
// In main.rs or when creating orchestrator
let orchestrator = WalletSyncOrchestrator::new(
    indexer_client,
    context,
    seed,
    network,
)?;
```

## Running the Service

### Prerequisites

1. **Rust and Cargo**

   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Midnight Proof Server**

   ```bash
   docker run -d -p 6300:6300 midnightnetwork/proof-server \
     -- 'midnight-proof-server --network testnet'
   ```

3. **Clone Repository**
   ```bash
   git clone https://github.com/shahnami/midnight-state-sync.git
   cd midnight-state-sync
   ```

### Basic Usage

```bash
# Run with default configuration (testnet)
cargo run
```

## Transaction Status Handling

The service properly handles different transaction states:

### ApplyStage Enum

```rust
pub enum ApplyStage {
    Pending,            // Transaction not yet finalized
    SucceedEntirely,    // Transaction fully successful
    SucceedPartially,   // Transaction partially successful
    FailEntirely,       // Transaction completely failed
}
```

Only transactions with `SucceededEntirely` or `SucceededPartially` status are applied to the wallet state, preventing corruption from failed transactions.

## Error Handling

The service includes comprehensive error handling for:

- Network connectivity issues
- Invalid transaction formats
- Merkle tree inconsistencies
- Failed transaction application
- Corrupted state recovery

## Development

### Project Structure

```
src/
├── main.rs                 # Entry point
├── wallet/
│   ├── sync/
│   │   ├── orchestrator.rs # Main sync coordinator
│   │   ├── strategies.rs   # Sync strategies
│   │   ├── events.rs       # Event system
│   │   ├── transaction_processor.rs
│   │   └── merkle_update_service.rs
│   └── types.rs
├── indexer/
│   ├── client.rs          # GraphQL client
│   └── types.rs           # Indexer types
└── transaction/           # Transaction building

```

### Debugging

Enable detailed logging:

```bash
RUST_LOG=midnight_state_sync=debug cargo run
```

Log levels:

- `error`: Critical errors only
- `warn`: Warnings and errors
- `info`: General information (default)
- `debug`: Detailed debugging info
- `trace`: Very verbose tracing
