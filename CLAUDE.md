# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Educational implementation of the Raft consensus protocol in Rust, built through "vibe coding" - iterative, conversational development with an AI assistant.

## Build & Test Commands

```bash
cargo build                              # Build the project
cargo test                               # Run all tests
cargo test <test_name>                   # Run a single test
cargo test -- --nocapture                # Run tests with stdout visible
cargo test --test http_cluster           # Run HTTP integration tests
cargo test --test linearizability_test   # Run linearizability tests
cargo test --package chaos-test          # Run chaos-test crate tests only
```

Run cluster locally:
```bash
./run_cluster.sh start         # Start 3-node cluster
./run_cluster.sh status        # Check cluster status
./run_cluster.sh stop          # Stop cluster
./run_cluster.sh killnode 1    # Kill node 1 to test failover
./run_cluster.sh startnode 1   # Restart node 1
./run_cluster.sh watchlogs     # Tail all logs with colored output
```

## Architecture

Layered architecture separating core Raft logic from async networking:

### Layer 1: RaftCore (`src/core/raft_core.rs`)
Synchronous, transport-agnostic Raft state machine:
- Persistent state: `current_term`, `voted_for`, `log`
- Volatile state: `commit_index`, `last_applied`, `next_index`, `match_index`
- RPC handlers: `handle_request_vote()`, `handle_append_entries()`, `handle_install_snapshot()`
- Election: `start_election()`, `become_leader()`, `handle_request_vote_result()`
- Replication: `append_log_entry()`, `handle_append_entries_result()`
- Snapshotting: `take_snapshot()`, log compaction

### Layer 2: Transport (`src/transport/`)
Async trait abstraction (`traits.rs`) for network communication:
- `HttpTransport` (`http.rs`) - Production HTTP-based transport
- `InMemoryTransport` (`inmemory.rs`) - Tokio channels for testing

### Layer 3: RaftNode (`src/core/raft_node.rs`)
High-level async operations combining RaftCore with Transport:
- Wraps `RaftCore` in `Arc<Mutex<_>>` for concurrent access
- `request_votes()` - Send vote requests to all peers concurrently
- `replicate_to_peers()` - Replicate entries to all peers
- `send_heartbeat()` - Send heartbeats (may include InstallSnapshot for far-behind followers)

### Layer 4: RaftServer (`src/core/raft_server.rs`)
Event-driven server loop with timeouts:
- Election timeout with randomization (300-500ms default)
- Heartbeat interval (150ms default)
- Client command handling via `RaftHandle`
- ReadIndex for linearizable reads

### Storage (`src/storage/`)
Pluggable persistence: `FileStorage` for production, `MemoryStorage` for tests.

### State Machine (`src/state_machine/`)
Pluggable state machines via trait. Includes `KeyValueStore` with `validate()` for pre-append command validation.

### Chaos Testing (`chaos-test/`)
Jepsen-like linearizability testing framework:
- WGL (Wing-Gong Linearizability) checker in `checker.rs`
- HTTP test client recording operations with timestamps in `client.rs`
- Test runner orchestrating concurrent clients in `runner.rs`
- `TestCluster` helper in `src/testing.rs` for spinning up test clusters

## Key Design Patterns

1. **Timeout reset via last_heartbeat**: `handle_append_entries()` updates `last_heartbeat = Instant::now()`. The server checks `has_election_timed_out()` to decide when to start elections.

2. **Lock discipline**: RaftNode releases the core lock before network I/O, reacquires to process responses.

3. **Concurrent RPC**: Uses `futures::future::join_all()` to send RPCs to all peers in parallel.

4. **Deterministic time testing**: Tests use `#[tokio::test(start_paused = true)]` with `tokio::time::advance()`.

5. **Log indexing with snapshots**: After snapshot at index S, `log[0]` contains entry S+1. Use `get_log_entry(index)` helper which accounts for `snapshot_last_index` offset.

## Testing Patterns

In-memory transport requires explicit request processing:
```rust
let (became_leader, _, _) = tokio::join!(
    node1.request_votes(),
    handle2.process_one_shared(&shared2),
    handle3.process_one_shared(&shared3),
);
```

For HTTP cluster tests, use `TestCluster` from `raft_vibe::testing`:
```rust
let cluster = TestCluster::new().await;
let leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
```

## Important Rules

- Write tests for each change
- Run tests after each change
- Always run tests wrapped with `timeout 30`
- Always fix Rust warnings (don't use `#[allow(dead_code)]`)
- Don't mention Claude in commit messages
- Update README.md after changes to architecture or project structure
- Document changes in LOG.md:
  ```markdown
  # Day X. Summary of what we did
  **Prompt:**: <prompt>
  **<Change title>**

  <Change explanation>
  ```
