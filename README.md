# Raft Consensus in Rust

An educational implementation of the [Raft consensus protocol](https://raft.github.io/raft.pdf) in Rust, built through "vibe coding" - iterative, conversational development with an AI assistant.

## About This Project

This project is designed for learning both Rust and distributed consensus concepts. It's not production-ready, but implements the core Raft features:

- **Leader Election** - Nodes elect a leader through randomized timeouts
- **Log Replication** - Leader replicates log entries to followers
- **Log Compaction** - Automatic snapshotting to prevent unbounded log growth
- **Linearizable Reads** - ReadIndex algorithm for consistent reads
- **Persistence** - File-based storage for crash recovery

## Quick Start

### Prerequisites

- Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`

### Build

```bash
cargo build --release
```

### Run Tests

```bash
cargo test
```

## Running a Cluster

### Using the Helper Script (Recommended)

```bash
# Start a 3-node cluster
./run_cluster.sh start

# Check cluster status
./run_cluster.sh status

# Watch logs (colored by node)
./run_cluster.sh watchlogs

# Kill a node to test failover
./run_cluster.sh killnode 1

# Restart a node
./run_cluster.sh startnode 1

# Stop the cluster
./run_cluster.sh stop

# Clean all data
./run_cluster.sh clean
```

### Manual Cluster Setup

Run each node in a separate terminal:

```bash
# Node 1
cargo run --release --bin raft-server -- \
    --id 1 --port 8001 --data-dir /tmp/raft1 \
    --peers 2=127.0.0.1:8002,3=127.0.0.1:8003

# Node 2
cargo run --release --bin raft-server -- \
    --id 2 --port 8002 --data-dir /tmp/raft2 \
    --peers 1=127.0.0.1:8001,3=127.0.0.1:8003

# Node 3
cargo run --release --bin raft-server -- \
    --id 3 --port 8003 --data-dir /tmp/raft3 \
    --peers 1=127.0.0.1:8001,2=127.0.0.1:8002
```

### Client API

```bash
# Check node status
curl http://127.0.0.1:8001/client/status

# Check who is leader
curl http://127.0.0.1:8001/client/leader

# Submit a command (must go to leader)
curl -X POST http://127.0.0.1:8001/client/submit \
     -H 'Content-Type: application/json' \
     -d '{"command": "SET mykey myvalue"}'

# Linearizable read
curl http://127.0.0.1:8001/client/read/mykey
```

## Architecture

The codebase follows a layered architecture separating core Raft logic from async networking:

```
┌─────────────────────────────────────────────────────────────┐
│                    HTTP Server (axum)                        │
│  Client API: /client/submit, /client/read, /client/status   │
│  Raft RPC:   /raft/request_vote, /raft/append_entries       │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                 RaftServer (raft_server.rs)                  │
│  Event loop with election timeouts, heartbeats, client cmds │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                  RaftNode (raft_node.rs)                     │
│  Async operations: request_votes(), replicate_to_peers()    │
└─────────────────────────────────────────────────────────────┘
                              │
         ┌────────────────────┴────────────────────┐
         │                                         │
┌────────────────────┐                 ┌───────────────────────┐
│ RaftCore           │                 │ Transport             │
│ (raft_core.rs)     │                 │ (transport/*.rs)      │
│                    │                 │                       │
│ Pure state machine │                 │ HttpTransport         │
│ - Election logic   │                 │ InMemoryTransport     │
│ - Log replication  │                 │                       │
│ - Snapshotting     │                 │                       │
└────────────────────┘                 └───────────────────────┘
         │
┌────────────────────┐
│ Storage            │
│ (storage/*.rs)     │
│                    │
│ FileStorage        │
│ MemoryStorage      │
└────────────────────┘
```

### Layer Details

| Layer | File | Description |
|-------|------|-------------|
| **RaftCore** | `src/core/raft_core.rs` | Synchronous, transport-agnostic state machine. Handles RequestVote, AppendEntries, elections, and log management. |
| **RaftNode** | `src/core/raft_node.rs` | Wraps RaftCore with async operations. Sends RPCs to all peers concurrently. |
| **RaftServer** | `src/core/raft_server.rs` | Event-driven server loop. Manages election timeouts (300-500ms), heartbeats (150ms), and client commands. |
| **Transport** | `src/transport/` | Network abstraction. `HttpTransport` for production, `InMemoryTransport` for testing. |
| **Storage** | `src/storage/` | Persistence layer. `FileStorage` for durability, `MemoryStorage` for testing. |
| **State Machine** | `src/state_machine/` | Pluggable state machines. Includes `KeyValueStore` implementation. |

## Project Structure

```
src/
├── bin/
│   └── server.rs           # Raft server binary
├── core/
│   ├── raft_core.rs        # Core Raft state machine
│   ├── raft_node.rs        # Async wrapper for RaftCore
│   ├── raft_server.rs      # Event-driven server loop
│   ├── config.rs           # Configuration builder
│   └── snapshot.rs         # Snapshot data structures
├── transport/
│   ├── traits.rs           # Transport trait definition
│   ├── http.rs             # HTTP transport implementation
│   └── inmemory.rs         # In-memory transport for testing
├── storage/
│   ├── traits.rs           # Storage trait definition
│   ├── file.rs             # File-based persistent storage
│   └── memory.rs           # In-memory storage for testing
├── state_machine/
│   ├── traits.rs           # State machine trait
│   └── kv.rs               # Key-value store implementation
├── api/
│   └── client_http.rs      # HTTP API for clients
└── lib.rs                  # Library exports
```

## Resources

- [Raft Paper](https://raft.github.io/raft.pdf) - The original paper
- [Raft Visualization](https://raft.github.io/) - Interactive visualization
- [Rust Book](https://doc.rust-lang.org/book/) - Learn Rust

## Development

See `CLAUDE.md` for development guidelines and `LOG.md` for session history.

```bash
cargo build              # Build the project
cargo test               # Run all tests
cargo test <test_name>   # Run a single test
cargo test -- --nocapture  # Run tests with stdout visible
```
