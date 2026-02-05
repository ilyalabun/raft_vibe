# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Educational implementation of the Raft consensus protocol in Rust, built through "vibe coding" - iterative, conversational development with an AI assistant.

## Build & Test Commands

```bash
cargo build                    # Build the project
cargo build --release          # Build for release
cargo test                     # Run all tests
cargo test <test_name>         # Run a single test
cargo test -- --nocapture      # Run tests with stdout visible
```

Run cluster locally:
```bash
./run_cluster.sh start         # Start 3-node cluster
./run_cluster.sh status        # Check cluster status
./run_cluster.sh stop          # Stop cluster
```

## Architecture

Layered architecture separating core Raft logic from async networking:

### Layer 1: RaftCore (`src/core/raft_core.rs`)
Synchronous, transport-agnostic Raft state machine:
- Persistent state: `current_term`, `voted_for`, `log`
- Volatile state: `commit_index`, `last_applied`, `next_index`, `match_index`
- RPC handlers: `handle_request_vote()`, `handle_append_entries()`
- Election: `start_election()`, `become_leader()`, `handle_request_vote_result()`
- Replication: `append_log_entry()`, `handle_append_entries_result()`

### Layer 2: Transport (`src/transport/`)
Async trait abstraction (`traits.rs`) for network communication:
- `HttpTransport` (`http.rs`) - Production HTTP-based transport
- `InMemoryTransport` (`inmemory.rs`) - Tokio channels for testing

### Layer 3: RaftNode (`src/core/raft_node.rs`)
High-level async operations combining RaftCore with Transport:
- Wraps `RaftCore` in `Arc<Mutex<_>>` for concurrent access
- `request_votes()` - Send vote requests to all peers concurrently
- `replicate_to_peers()` - Replicate entries to all peers

### Layer 4: RaftServer (`src/core/raft_server.rs`)
Event-driven server loop with timeouts:
- Election timeout with randomization (300-500ms default)
- Heartbeat interval (150ms default)
- Client command handling via `RaftHandle`

### Storage (`src/storage/`)
Pluggable persistence: `FileStorage` for production, `MemoryStorage` for tests.

### State Machine (`src/state_machine/`)
Pluggable state machines via trait. Includes `KeyValueStore` implementation.

## Key Design Patterns

1. **Event-based timeout reset**: `handle_append_entries()` returns `RaftEvent::ResetElectionTimeout` to signal the server layer should reset its election timer.

2. **Lock discipline**: RaftNode releases the core lock before network I/O, reacquires to process responses.

3. **Concurrent RPC**: Uses `futures::future::join_all()` to send RPCs to all peers in parallel.

4. **Deterministic time testing**: Tests use `#[tokio::test(start_paused = true)]` with `tokio::time::advance()`.

## Testing Pattern

In-memory transport requires explicit request processing:
```rust
let (became_leader, _, _) = tokio::join!(
    node1.request_votes(),
    handle2.process_one_shared(&shared2),
    handle3.process_one_shared(&shared3),
);
```

## Important Rules

- Write tests for each change
- Run tests after each change
- Always run tests wrapped with `timeout 30`
- Always fix Rust warnings (don't use `#[allow(dead_code)]`)
- Update README.md after changes to architecture or project structure
- Document changes in LOG.md:
  ```markdown
  # Day X. Summary of what we did
  **Prompt:**: <prompt>
  **<Change title>**

  <Change explanation>
  ```
