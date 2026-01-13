# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Educational implementation of the Raft consensus protocol in Rust. The project is designed for learning both Rust and distributed consensus concepts through "vibe coding" - iterative, conversational development with an AI assistant.

## Build & Test Commands

```bash
cargo build              # Build the project
cargo run                # Run the demo (synchronous Raft simulation)
cargo run --example basics  # Run Rust basics examples
cargo test               # Run all tests
cargo test <test_name>   # Run a single test
cargo test -- --nocapture  # Run tests with stdout visible
```

## Architecture

The codebase follows a layered architecture separating core Raft logic from async networking:

### Layer 1: RaftCore (`src/raft_core.rs`)
Synchronous, transport-agnostic Raft state machine. Contains:
- Persistent state: `current_term`, `voted_for`, `log`
- Volatile state: `commit_index`, `last_applied`, `next_index`, `match_index`
- RPC handlers: `handle_request_vote()`, `handle_append_entries()`
- Election: `start_election()`, `become_leader()`, `handle_request_vote_result()`
- Replication: `append_log_entry()`, `handle_append_entries_result()`

### Layer 2: Transport (`src/transport.rs`)
Async trait abstraction for network communication:
- `request_vote(target, args)` - Send RequestVote RPC
- `append_entries(target, args)` - Send AppendEntries RPC

In-memory implementation in `src/transport_inmemory.rs` uses tokio channels for testing.

### Layer 3: RaftNode (`src/raft_node.rs`)
High-level async operations combining RaftCore with Transport:
- Wraps `RaftCore` in `Arc<Mutex<_>>` for concurrent access
- `request_votes()` - Send vote requests to all peers concurrently
- `replicate_to_peers()` - Replicate entries to all peers
- `send_heartbeat()` - Leader heartbeat with catch-up entries

### Layer 4: RaftServer (`src/raft_server.rs`)
Event-driven server loop with timeouts:
- Election timeout with randomization (300-500ms default)
- Heartbeat interval (150ms default)
- Client command handling via `RaftHandle`
- Uses `ServerEvent::ResetElectionTimeout` from transport layer

### Configuration (`src/config.rs`)
`RaftConfig` builder pattern for timing parameters.

## Key Design Patterns

1. **Event-based timeout reset**: `handle_append_entries()` returns `RaftEvent::ResetElectionTimeout` to signal the server layer should reset its election timer.

2. **Lock discipline**: RaftNode releases the core lock before network I/O, reacquires to process responses.

3. **Concurrent RPC**: Uses `futures::future::join_all()` to send RPCs to all peers in parallel.

4. **Test-util time**: Tests use `#[tokio::test(start_paused = true)]` with `tokio::time::advance()` for deterministic timing tests.

## Testing Pattern

The in-memory transport uses a pattern where request processing must be explicitly driven:
```rust
let (became_leader, _, _) = tokio::join!(
    node1.request_votes(),
    handle2.process_one_shared(&shared2),
    handle3.process_one_shared(&shared3),
);
```

## Important things

- Write tests for each change
- Run tests after each change
- Split big changes into more manageable smaller changes
- Always document what we did in LOG.md in following format:
  ```markdown

  # Day X. Summary of what we did
  **Prompt:**: <prompt>
  **<Change title>**

  <Change explanation>
  ```

