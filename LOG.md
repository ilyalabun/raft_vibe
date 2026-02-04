# Vibe Coding Log - Learning Raft in Rust

A journal of my journey learning Rust and the Raft consensus protocol through interactive AI-assisted coding.

---

## Day 1: Project Setup and Foundation

**Prompt:** "Hello. I want to learn Rust and also Raft consensus protocol. Please guide me through Raft implementation in Rust, I have almost 0 Rust knowledge. I'm pretty new to vibe coding, so some vibe coding basics in the beginning are highly appreciated."

**Setting Up the Learning Environment**

Started a new project to learn both Rust and the Raft consensus protocol from scratch. Created a complete Rust project structure with Cargo, including a main implementation file and examples directory. The goal was to build a working Raft implementation while learning Rust fundamentals along the way.

**Creating Learning Materials**

Built comprehensive documentation to support the learning journey. Created a README with project overview and learning path, a detailed Rust basics tutorial covering ownership, borrowing, structs, enums, and common patterns, a vibe coding guide explaining how to effectively work with an AI assistant, and a quick start guide for getting up and running in minutes. These materials serve as both reference and teaching tools.

**Implementing Core Raft Algorithm**

Implemented a complete Raft consensus algorithm from scratch. Created all essential data structures including RaftState enum (Follower, Candidate, Leader), LogEntry struct, and RPC message types (RequestVote and AppendEntries). Implemented the core state machine with methods for handling elections, log replication, and state transitions. The implementation follows the Raft paper specification with proper term management and log consistency checks.

**Building a Working Demo**

Created a demonstration program that shows a 3-node Raft cluster in action. The demo walks through a complete scenario: nodes start as followers, one node initiates an election and becomes leader after receiving votes, then the leader replicates a log entry to all followers. This provides a concrete example of how Raft works in practice and validates the implementation.

**Adding Examples and Tests**

Created Rust basics examples demonstrating key concepts like variables, structs, enums, vectors, and options. Wrote unit tests for the core Raft functionality including node creation, elections, vote handling, and log replication. All tests pass, confirming the implementation works correctly.

**Prompt:** "what's #[derive(Debug, Clone, Copy, PartialEq, Eq)]"

**Understanding Rust Attributes**

Explored the `#[derive]` attribute in Rust, which automatically generates trait implementations. Learned about Debug (for printing), Clone and Copy (for copying values), and PartialEq and Eq (for equality comparisons). Created detailed documentation explaining each trait and when to use them, with practical examples from the Raft codebase.

**Prompt:** "what's the diff between PartialEq and Eq"

**Deep Dive: PartialEq vs Eq**

Investigated the difference between PartialEq and Eq traits in Rust. Discovered that PartialEq allows equality comparison but doesn't guarantee total equality (like with floating-point NaN), while Eq extends PartialEq to guarantee complete mathematical equality properties. Created a working example demonstrating why floating-point numbers only have PartialEq, while enums like RaftState can have both traits. This understanding is crucial for using types as HashMap keys or in other contexts requiring total equality.

**Prompt:** "Also, I want to write series of blog posts about my vibe coding experience. So I want you to document everything in LOG.md without diff, just what we've done in a couple of sentenses"

**Documenting the Journey**

Created a LOG.md file to document the entire vibe coding session as a series of blog post entries. Each entry captures what was accomplished in a few sentences, written in a narrative style suitable for blog posts. The log serves as a timeline of the learning journey and can be expanded into full blog posts.

**Prompt:** "Please add prompts I used"

**Completing the Documentation**

Added all the prompts used during the session to the log, showing the natural flow of questions and learning. This demonstrates how vibe coding works - starting with a broad goal, then diving deeper into specific concepts as they come up during exploration.

**Prompt:** "there are bunch of warning when I do `cargo run`. please fix them. Warnings are mostly about never read fields" and "no, please don't allow dead code"

**Fixing Compiler Warnings the Right Way**

Encountered compiler warnings about unused fields in the Raft implementation. Instead of suppressing them with `#[allow(dead_code)]`, implemented proper usage of all fields according to the Raft protocol. Added methods to process RPC responses that check and update terms (`process_request_vote_response` and `process_append_entries_response`), implemented `apply_committed_entries` to update `last_applied` when entries are committed, and enhanced the demo to show the complete Raft flow including committing entries and applying them to the state machine. The code now compiles without warnings and demonstrates a more complete Raft implementation.

**Prompt:** "why do we need `vote_args.clone()` and such?" and "can we do clone automatically?" and "can we avoid doing clone() during iteration somehow?"

**Optimizing Ownership with References**

Learned about Rust's ownership system and how to avoid unnecessary cloning. Changed all RPC handler functions to accept references (`&RequestVoteArgs`, `&AppendEntriesArgs`, etc.) instead of taking ownership, eliminating the need for `.clone()` calls when passing arguments to multiple nodes. For iterating over entries, used `for entry in &args.entries { self.log.push(entry.clone()); }` to borrow the vector and only clone individual entries when needed, rather than cloning the entire vector upfront. This approach is more efficient and demonstrates proper Rust ownership patterns.

**Prompt:** "Why code I added works? I expect follower to decline append commands because it's not leader"

**Enforcing Raft Protocol: Leader-Only Log Appends**

Discovered a critical bug where followers could accept client commands, violating the Raft protocol. Fixed `append_log_entry` to check if the node is a leader before allowing log appends. Changed the return type to `Option<LogEntry>` - returns `Some(entry)` for leaders and `None` for followers. Updated the demo to show that followers properly reject client commands with a clear message. This ensures only leaders accept client requests, which is fundamental to Raft's consistency guarantees. The fix demonstrates how Rust's type system (using `Option`) helps enforce protocol correctness at compile time.

## Day 2: Encapsulation and Better Abstractions

**Prompt:** "Now, I want to encapsulate election, becoming leader, etc logic from `main.rs`. For this following methods are missing in RaftNode: handle_append_entries_result, handle_request_vote_result"

**Encapsulating Vote and Replication Logic**

Refactored the Raft implementation to better encapsulate election and replication logic within the `RaftNode` struct. Added a `votes_received` field to track which peers have granted votes during an election. Created `handle_request_vote_result` method that processes vote responses, tracks votes, and automatically transitions to leader when a majority is reached. Similarly, created `handle_append_entries_result` method that handles replication responses, updates `match_index` and `next_index` for each peer, and commits entries when replicated to a majority. Also improved code readability by using clearer variable names like `vote_req`, `append_req`, and `candidate_last_log_term`. The main demo was simplified to use these higher-level methods instead of manually tracking votes and replication counts. This refactoring follows better software design principles by keeping related logic together and reducing code duplication.

## Day 3: Transport Abstraction

**Prompt:** "I want to introduce simple Transport abstraction. It should use tokio async library and have 2 methods: request_vote and append_entries"

**Creating the Transport Trait**

Introduced an async transport abstraction for Raft RPC communication. Added `tokio` and `async-trait` dependencies to enable async/await syntax in traits. Created a `Transport` trait with two async methods: `request_vote` and `append_entries`, both returning `Result` types with a custom `TransportError` enum for error handling (ConnectionFailed, Timeout, NodeNotFound). This abstraction allows different transport implementations (TCP, gRPC, in-memory) to be swapped without changing the Raft logic.

**Prompt:** "please extract transport into separate file"

**Modularizing the Codebase**

Extracted the `Transport` trait and `TransportError` enum into a dedicated `src/transport.rs` file. Updated imports to reference the raft module's RPC types. This separation of concerns makes the codebase more maintainable and clearly separates the transport layer from the consensus algorithm.

**Prompt:** "ok, now I want simplest in-memory transport implementation. It should live in a separate transport_inmemory.rs file. Implementation should be the simplest, try to avoid locks if possible. Write tests for this implementation"

**Implementing Lock-Free In-Memory Transport**

Created an in-memory transport implementation using tokio channels instead of locks. The design uses `mpsc` channels for sending requests and `oneshot` channels for receiving responses. Each node has a `NodeHandle` that receives requests and processes them with its `RaftNode`. The `InMemoryTransport` holds senders to all peer nodes. Added a `create_cluster` helper function to easily set up a test cluster. Wrote four tests covering RequestVote, AppendEntries, error handling for unknown nodes, and a full election flow through the transport layer. All 8 tests pass.

**Prompt:** "what does ? mean in rust?"

**Understanding Error Propagation**

Learned about Rust's `?` operator for error propagation. It's shorthand for matching on a `Result` - if `Ok`, unwrap the value and continue; if `Err`, return early from the function with that error. The function must return a compatible `Result` type for `?` to work.

**Prompt:** "why we don't need lock for RaftNode in process_one?"

**Understanding Rust's Ownership Model**

Explored why locks aren't needed when using `&mut RaftNode`. Rust's borrow checker guarantees at compile time that only one mutable reference can exist at a time. The caller is responsible for exclusive access, and any attempt to use the node elsewhere while borrowed would be rejected by the compiler. Locks would only be needed if multiple tasks shared ownership via `Arc<Mutex<RaftNode>>`.

## Day 4: RaftServer and Architecture Refactoring

Created a generic RaftServer and refactored the codebase into a clean three-layer architecture: RaftCore (sync state machine), RaftNode (async consensus), and RaftServer (client API). Fixed async deadlock issues and learned about proper concurrent RPC handling.

**Prompt:** "Yes, please create RaftServer. Try to make it generic, so it'll be easier to change transport"

**Creating Generic RaftServer**

Created a generic `RaftServer<T: Transport>` that handles client commands and coordinates with the Raft consensus layer. The server spawns a background tokio task to process client commands. It provides a `RaftHandle` for clients to submit commands asynchronously. The server uses `SharedCore` (`Arc<Mutex<RaftCore>>`) to share state between the server and incoming RPC handlers. Fixed deadlock issues by sending all RPC requests concurrently using `futures::future::join_all` instead of sequentially. Added the `futures` crate dependency.

**Prompt:** "Should handle_submit and replicate_to_peers be part of RaftNode?"

**Discussing Code Organization**

Discussed where consensus logic should live. The argument for keeping replication in RaftServer was that RaftCore is sync and transport-agnostic. But replication is core to Raft consensus - a leader must replicate to achieve consensus. Decided to create a layered architecture with consensus logic in a separate wrapper.

**Prompt:** "RaftCore sounds good!"

**Refactoring to Three-Layer Architecture**

Refactored the codebase into a cleaner three-layer architecture:
- `RaftCore` (raft_core.rs) - Sync state machine, transport-agnostic. Handles RPCs and state transitions.
- `RaftNode<T: Transport>` (raft_node.rs) - Async consensus logic. Wraps RaftCore and contains `request_votes()` and `replicate_to_peers()`. Generic over transport.
- `RaftServer<T: Transport>` (raft_server.rs) - Client command handling. Contains RaftNode and coordinates client requests.

This separation keeps concerns clean: state management, consensus protocol, and client API are each in their own layer. The generic transport parameter makes it easy to swap implementations.

## Day 5: Heartbeat and Election Timeout

**Prompt:** "ok, now I want to implement Raft's heartbit functionality"

**Implementing Heartbeat Functionality**

Implemented Raft's heartbeat mechanism, which is essential for leader authority and follower liveness. Created a `RaftConfig` struct in a new `config.rs` module to hold timing parameters (heartbeat interval, election timeout min/max). Added `send_heartbeat()` method to `RaftNode` that sends AppendEntries RPCs to all peers - importantly, heartbeats include any missing log entries to help followers catch up, not just empty messages. Updated `RaftServer`'s main loop to use `tokio::select!` for handling multiple concurrent concerns: client commands, heartbeat timer (fires every 150ms for leaders), and election timeout.

**Prompt:** "Can heartbeat replicate existing entries to 'catch up' followers?"

**Heartbeats as Catch-Up Mechanism**

Confirmed that heartbeats should include missing entries for followers that are behind. Modified `send_heartbeat()` to check each peer's `next_index` and include any entries they're missing. This is how Raft efficiently catches up slow or recovering followers without needing a separate catch-up mechanism.

**Prompt:** "Please explain how election_timeout_reset works?" and "Let's explore approach #2"

**Event-Based Election Timeout Reset**

Explored different approaches for resetting election timeouts when heartbeats arrive. Chose an event-based design where `handle_append_entries()` returns a `HandleAppendEntriesOutput` struct containing both the RPC result and a `RaftEvent` enum. When a valid AppendEntries is received (not stale term), it returns `RaftEvent::ResetElectionTimeout`. Added a `ServerEvent` channel so the transport layer can notify the server to reset its election deadline. The server loop uses `tokio::time::sleep_until()` with a deadline that gets pushed forward when reset events arrive.

**Prompt:** "cool, now I want to test that RaftNode initiates election when election timeout passed"

**Testing Election Timeout Behavior**

Wrote integration tests for election timeout behavior. First test verifies that a follower automatically starts an election and becomes leader after the timeout expires. Second test verifies that a follower does NOT start an election before the timeout. Initially used real timing but this made tests slow and potentially flaky.

**Prompt:** "messing with real timings can make tests flacky. Is there any way to avoid it?"

**Deterministic Time Testing with Tokio**

Refactored tests to use Tokio's time mocking instead of real delays. Added `test-util` feature to tokio dependency. Used `#[tokio::test(start_paused = true)]` to pause time at test start, then `tokio::time::advance()` to manually move time forward and `tokio::task::yield_now()` to let tasks run. Tests now run instantly regardless of configured timeout values, and are fully deterministic - no more flaky timing issues in CI.

## Day 6: Comprehensive Test Coverage

**Prompt:** "Let's analyze more test cases and write more tests"

**Expanding Test Coverage**

Added extensive tests to cover edge cases and failure scenarios. Wrote tests for vote rejection (candidate with lower term, already voted, shorter log), term/state transitions (follower updates term, candidate steps down), and leader replication logic (next_index, match_index, majority commit). Fixed two bugs discovered during testing: AppendEntries wasn't idempotent (duplicate entries were appended), and the leader could commit previous-term entries directly (violating Raft Section 5.4.2 safety property).

**Prompt:** "let's add timeouts to tests"

**Adding Timeout Support to Transport**

Extended the in-memory transport with timeout support for more realistic testing. Added `InMemoryTransport::with_timeout()` and `create_cluster_with_timeout()` helpers. Tests can now simulate network delays and partial responses. Used `#[tokio::test(start_paused = true)]` with `tokio::time::advance()` for deterministic timing without real delays.

**Prompt:** "let's do RaftServer and network partition tests"

**Network Partition and Client Command Tests**

Added integration tests for RaftServer covering the full client command flow (submit → replicate → commit), multiple commands in sequence, leader failover, leader isolated from majority, split-brain prevention, partitioned node rejoining, and stale leader stepping down. Learned the pattern for testing with spawned tasks and paused time: spawn both the submit task and peer handlers, then advance time in a loop to let all tasks make progress.

**Prompt:** "let's cover candidate rejects commands and command fails mid-replication"

**Final Edge Case Tests**

Added tests verifying that candidates reject client commands (not just followers), and that commands fail properly when a leader loses leadership mid-operation (e.g., sees a higher term during replication). Test suite now has 86 tests covering core Raft logic, async operations, and failure scenarios.

## Day 7: Persistence Layer

**Prompt:** "let's work on persistence. I want to implement persistence behind some interface to be able to run unit tests quickly without side effects. what do you think?"

**Designing the Storage Abstraction**

Discussed the design for a persistence layer that abstracts storage behind a trait. Raft requires three pieces of state to be persisted before responding to RPCs: `current_term`, `voted_for`, and `log[]`. Created a `Storage` trait with methods for loading/saving these values, plus fine-grained log operations (`append_log_entries`, `truncate_log`). Added `StorageError` enum for error handling. Chose to use `Box<dyn Storage>` (trait objects) instead of generics to keep `RaftCore` non-generic and avoid complexity propagation through the codebase.

**Prompt:** "can we avoid generics in RaftCore?" and "what about error handling for storage operations?"

**Implementation Decisions**

Decided to use trait objects (`Box<dyn Storage>`) to keep `RaftCore` simple and non-generic. For error handling, chose to panic on storage failures - this is correct for Raft since failing to persist before responding violates protocol safety. In production, a node that can't persist should crash rather than risk inconsistency. Tests use `MemoryStorage` which never fails.

**Implementing the Storage Layer**

Created `src/storage.rs` with the `Storage` trait and `StorageError` type. Created `src/storage_memory.rs` with `MemoryStorage` implementation - a fast, in-memory storage perfect for unit tests. Updated `RaftCore` to take a `Box<dyn Storage>` in its constructor and load initial state from storage. Added persistence helper methods (`set_term`, `set_voted_for`, `update_term`, `persist_log_entry`, `persist_truncate_log`) that update both in-memory state and storage atomically. Modified all methods that change persistent state to use these helpers. Added `Send` bound to `Storage` trait for async compatibility.

**Updating Tests**

Created a `new_test_core()` helper function in each test module to create `RaftCore` with `MemoryStorage`. Updated all 92 tests across `raft_core.rs`, `raft_node.rs`, `raft_server.rs`, and `transport_inmemory.rs` to use the new constructor. All tests pass.

**Prompt:** "ok, now I want tests for persistence on node restart"

**Persistence Restart Tests**

Added 8 tests verifying that nodes correctly restore state after a restart. Tests cover: loading saved term, voted_for, and log entries individually and together; verifying volatile state (commit_index, last_applied, leadership) resets on restart; ensuring a restarted node respects its previous vote and can continue participating in elections; and confirming a former leader restarts as follower and must re-establish leadership. Test suite now has 100 tests.

**Prompt:** "let's think of possible *real* persistence implementation"

**Exploring Storage Options**

Discussed various approaches for real persistence: simple file-based (separate files for term, voted_for, log), single file with JSON/bincode, write-ahead log (WAL), SQLite, and sled. Decided to implement file-based storage first for learning purposes - it reveals the challenges of crash-safe persistence.

**Prompt:** "show me file-based implementation first"

**File-Based Storage Implementation**

Created `FileStorage` in `storage_file.rs` using three files: `term`, `voted_for`, and `log` (JSON lines format). Discussed the trade-offs of atomic writes (write to temp + rename) vs direct writes for small data. For tiny files like term (few bytes), direct write + fsync is effectively atomic at the disk block level.

**Prompt:** "but what about partial block write? Is it possible?"

**Understanding Disk Atomicity**

Explored the nuances of disk write atomicity. Modern drives have capacitors to complete sector writes on power loss, and most guarantee 512-byte sector atomicity - but it's not guaranteed by the POSIX spec. Real databases use checksums to detect any corruption.

**Prompt:** "let's do with checksums"

**Adding CRC32 Checksums**

Implemented CRC32 checksums (IEEE polynomial, same as zlib/gzip) for all persistent data. File format: `{data} {crc32_hex}\n`. On load, verify checksum and return `StorageError::Corruption` if mismatch. Added serde for JSON serialization and tempfile for tests. Added corruption detection tests. Test suite now has 109 tests.

## Day 8: State Machine

**Prompt:** "ok, new day. let's continue"

**Designing the State Machine Abstraction**

Implemented a pluggable state machine abstraction for Raft. Created a `StateMachine` trait in `state_machine.rs` with a single `apply(command: &str) -> ApplyResult` method where `ApplyResult = Result<String, String>`. The trait requires `Send` for async compatibility. Added `TestStateMachine` that records all applied commands for verification in tests.

**Implementing KeyValueStore**

Created `KeyValueStore` in `state_machine_kv.rs` - a simple in-memory key-value store implementing `StateMachine`. Only state-changing commands go through `apply()`: `SET key value` and `DELETE key`. Reads use the direct `get()` method which bypasses Raft - this is correct because reads don't change state and shouldn't be replicated. Uses `splitn(3, ' ')` to allow values with spaces.

**Integrating State Machine into RaftCore**

Updated `RaftCore` to hold a `Box<dyn StateMachine>` alongside the storage. Modified `apply_committed_entries()` to actually call the state machine's `apply()` method when entries are committed. `TestStateMachine` uses `Arc<Mutex<Vec<String>>>` to allow tests to verify exactly which commands were applied. Added integration tests verifying: (1) commands are applied to state machine when quorum is reached, (2) commands are NOT applied when leader is partitioned, and (3) followers apply commands after receiving heartbeat with updated commit_index. Test suite now has 122 tests.

## Day 9: HTTP Transport

**Prompt:** "ok, today I want to work on http transport"

**Choosing HTTP Stack**

Decided to use axum (from the Tokio team) for the HTTP server and reqwest for the HTTP client, with JSON serialization via serde. This combination is well-supported in the Rust async ecosystem and provides a clean API for building HTTP services.

**Implementing HttpTransport**

Created `transport_http.rs` with a complete HTTP transport implementation:
- `HttpTransport` struct implementing the `Transport` trait, holding a map of peer addresses and a reqwest client with configurable timeout
- Two endpoints: `POST /raft/request_vote` and `POST /raft/append_entries`
- axum router with shared state (`SharedCore = Arc<Mutex<RaftCore>>`) for handling incoming RPCs
- Proper error mapping from reqwest errors to `TransportError` (timeout vs connection failed)

**Adding Serde to RPC Types**

Added `Serialize, Deserialize` derives to all RPC message types in `raft_core.rs` (`RequestVoteArgs`, `RequestVoteResult`, `AppendEntriesArgs`, `AppendEntriesResult`) to enable JSON serialization over HTTP.

**TLS Backend Decision**

Build initially failed because reqwest defaults to native-tls which requires OpenSSL system libraries. Chose rustls-tls instead for several reasons: it's memory-safe (written in Rust), has no system dependencies (pure Rust), has a smaller attack surface than OpenSSL, and refuses deprecated protocols by default. Updated Cargo.toml to use `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`.

**Testing**

Added 4 tests for the HTTP transport covering request_vote, append_entries, timeout handling (connecting to non-existent server), and node not found errors. All 126 tests pass.

## Day 9 (continued): Client HTTP API

**Prompt:** "ok, let's do client http api"

**Tracking Current Leader**

Added `current_leader: Option<u64>` field to RaftCore to track the known leader. This is updated when receiving valid AppendEntries from a leader, cleared when starting an election (challenging leadership), and set to self when becoming leader. This allows followers to provide leader hints to clients.

**Propagating State Machine Results**

Redesigned the command flow to return state machine results instead of just log indices. Modified `apply_committed_entries()` to return `Vec<(u64, Result<String, String>)>` containing index and result for each applied entry. Updated `handle_append_entries_result()` to return `(Option<u64>, Vec<...>)` - both the commit index and apply results. Modified `replicate_to_peers()` to return `Option<Result<String, String>>` - the result for the specific submitted entry if committed.

**Updated RaftError**

Extended `RaftError` enum with new variants: `NotLeader { leader_hint: Option<u64> }` includes a hint about who the leader is, `NotCommitted` for when an entry couldn't be committed to majority, and `StateMachine(String)` for state machine errors.

**Client HTTP Endpoints**

Created `client_http.rs` with three endpoints:
- `POST /client/submit` - Submit a command to the cluster. Returns state machine result or error with leader hint.
- `GET /client/leader` - Query current leader info (leader_id, node_id, is_leader).
- `GET /client/status` - Get node status (state, term, commit_index, last_applied, log_length).

**Testing**

Added 6 tests for the client HTTP API covering: submit when not leader, submit as leader, leader endpoint for follower/leader, status endpoint, and leader hint in error responses. All 132 tests pass.

## Day 10: Election Timeout Refactoring and Cluster Management

**Prompt:** "we were refactoring raft_core and raft_server to encapsulate leader election timeout event"

**Encapsulating Election Timeout in RaftCore**

Refactored election timeout handling to be fully encapsulated within `RaftCore`. Changed `last_heartbeat` from `std::time::Instant` to `tokio::time::Instant` to enable time mocking in tests. Instead of returning a `RaftEvent` from `handle_append_entries()`, the method now directly updates `last_heartbeat = Instant::now()`. Removed the `RaftEvent` enum entirely. Updated `start_election()` to also reset `last_heartbeat` to prevent infinite election loops when elections timeout repeatedly.

**Simplifying RaftServer**

Removed the `ServerEvent` channel mechanism from `RaftServer`. The server loop now checks `last_heartbeat` directly via `get_election_deadline()` and `has_election_timed_out()` helper methods. This removes complexity and keeps timing logic consolidated - the server queries RaftCore's state rather than receiving events.

**Prompt:** "can you add killnode and startnode options to run_cluster.sh?"

**Cluster Management Commands**

Enhanced `run_cluster.sh` with commands for simulating node failures:
- `killnode <id>` - Kill a specific node (1, 2, or 3)
- `startnode <id>` - Restart a killed node with preserved data directory
- `watchlogs` - Tail all logs in a single stream with colored output (Node 1=Blue, Node 2=Green, Node 3=Yellow)

**Prompt:** "I want you to print [NODE X] in node's logs"

**Adding Logging to Raft Operations**

Added logging throughout the Raft implementation with `[NODE X]` prefix:
- Leadership changes: "Became CANDIDATE", "Became LEADER", "Stepped down to FOLLOWER"
- Log appends: "Appended entry..." (leader) and "Replicated entry..." (follower)
- Commits: "Committed entry..."
- Updated `server.rs` startup messages to also use the prefix

**Prompt:** "I also need http endpoints to read state machine state"

**KV Read Endpoints**

Added HTTP endpoints to read state machine state directly:
- `GET /client/get/{key}` - Get a single key's value
- `GET /client/kv` - Dump entire key-value state as JSON

Created `SharedKvStore = Arc<Mutex<KeyValueStore>>` to share state machine between Raft and HTTP handlers. Implemented `StateMachine` trait for `SharedKvStore` to delegate to the inner store. Updated `server.rs` to use shared state machine and added the new routes.

**Prompt:** "I noticed that when cluster starts state machine is empty"

**Discovered State Machine Recovery Issue**

Identified a known Raft issue: on restart, the state machine is empty until a write happens because `commit_index` starts at 0 (it's volatile state) and leaders can only commit entries from the current term. The proper Raft solution is to append a no-op entry when a leader is elected, which commits all prior entries indirectly. This is a future enhancement.

## Day 11: No-Op Entry on Leader Election

**Prompt:** "ok, let's continue day 11 and implement no-op message"

**Implementing No-Op Entry**

Implemented the no-op entry feature to fix state machine recovery after leader election. Added a `NOOP_COMMAND` constant to `raft_core.rs`. Modified `become_leader()` to first initialize `next_index` and `match_index` for all peers (pointing to the last log index before the no-op), then append the no-op entry. This ordering (Option B) is faster because peers' `next_index` already points to the correct position for immediate replication.

**Handling NOOP in State Machine**

Updated `KeyValueStore` to recognize and handle the NOOP command - it simply returns an empty string without modifying state. This allows the no-op to flow through the state machine without errors.

**Test Infrastructure Improvements**

Added `drain_pending()` method to `NodeHandle` in the in-memory transport. This clears leftover requests in the channel, simulating requests being lost due to network timeout/partition. Essential for testing scenarios where some requests time out and new requests need to be processed cleanly.

**Updating Existing Tests**

Updated many tests across `raft_core.rs`, `raft_node.rs`, `raft_server.rs`, and `client_http.rs` to account for the no-op entry at index 1. Log lengths, commit indices, and applied command counts all shift by 1 because leaders now always have at least the NOOP entry.

**Prompt:** "what scenarios we should also consider to test this functionality?"

**Recovery Scenario Tests**

Added comprehensive tests for no-op behavior:

1. `test_uncommitted_entries_committed_via_noop` - 5-node cluster where leader 1 replicates entries to only 1 follower (2/5 = minority, not committed). Leader 1 crashes, follower 2 (which has the entries) becomes leader 2 and appends its own NOOP. When NOOP commits with majority, all prior entries are committed indirectly.

2. `test_fresh_node_catches_up_from_empty` - Node 3 starts with empty log while leader has committed entries. Via heartbeats, node 3 catches up on all entries including NOOP.

**Key Technical Discoveries**

- Multiple heartbeats are needed for log catch-up because `next_index` starts at the leader's log end and decrements on each rejection until it reaches index 1 where entries can be sent.
- 5-node cluster needed for minority replication tests (2/5 < majority of 3).
- `#[tokio::test(start_paused = true)]` auto-advances time for tokio timeouts, making timeout tests deterministic.

Test suite now has 134 tests, all passing.

**Prompt:** "I see that project structure is pretty flat. can you suggest more hierarchical structure to better organize source code?"

**Restructuring to Hierarchical Modules**

Reorganized the flat `src/*.rs` structure into a hierarchical module layout:

```
src/
├── lib.rs                    # Library root
├── main.rs                   # Demo entry point
├── core/                     # Core Raft consensus logic
│   ├── mod.rs, config.rs, raft_core.rs, raft_node.rs, raft_server.rs
├── storage/                  # Persistence layer
│   ├── mod.rs (trait), memory.rs, file.rs
├── transport/                # Network layer
│   ├── mod.rs (trait), inmemory.rs, http.rs
├── state_machine/            # Replicated state machines
│   ├── mod.rs (trait), kv.rs
└── api/                      # External APIs
    ├── mod.rs, client_http.rs
```

Moved all files to appropriate subdirectories and updated imports throughout. Traits are defined in `mod.rs` of each module, with implementations in sibling files. This separation makes it easier to add new implementations (e.g., `transport/grpc.rs`, `storage/sqlite.rs`) and keeps related code together.

---

## Day 12: Log Compaction / Snapshotting

**Prompt:** "let's implement log compaction via snapshotting"

**Designing the Snapshot System**

Implemented log compaction via snapshotting as described in Raft paper Section 7. Created a plan covering the full implementation: snapshot types, state machine serialization, storage persistence, transport RPC, and leader-to-follower snapshot transfer.

**Snapshot Types**

Created `src/core/snapshot.rs` with `SnapshotMetadata` (last_included_index, last_included_term) and `Snapshot` (metadata + serialized state machine data). Added `InstallSnapshotArgs` and `InstallSnapshotResult` RPC types to `raft_core.rs`.

**Snapshotable State Machine**

Extended `StateMachine` trait with `snapshot()` and `restore()` methods. Implemented for `KeyValueStore` using serde_json serialization. The snapshot captures the entire KV store state which can be restored on a follower.

**Storage Layer Updates**

Added `load_snapshot()` and `save_snapshot()` methods to the `Storage` trait. Implemented in both `MemoryStorage` (in-memory) and `FileStorage` (separate `snapshot` file with CRC32 checksum like other persisted state).

**Transport Layer Updates**

Added `install_snapshot()` method to the `Transport` trait. Implemented in both `InMemoryTransport` (channel-based) and `HttpTransport` (new `/raft/install_snapshot` endpoint).

**RaftCore Snapshot Integration**

Added `snapshot_last_index` and `snapshot_last_term` fields to track the last entry included in the most recent snapshot. Updated `last_log_index()` and `last_log_term()` to use snapshot values when the log is empty. Added helper `get_log_entry(index)` that accounts for snapshot offset when accessing log entries. Implemented `take_snapshot()` to serialize state machine, save to storage, and compact the log. Implemented `handle_install_snapshot()` to restore state machine from leader's snapshot.

**Leader Snapshot Replication**

Updated `send_heartbeat()` in `RaftNode` to detect when a follower is too far behind (next_index <= snapshot_last_index) and send `InstallSnapshot` instead of `AppendEntries`. After successful snapshot installation, updates `next_index` and `match_index` for the peer.

**Automatic Snapshot Triggering**

Added `snapshot_threshold` to `RaftConfig` (default 1000 entries). Modified `apply_committed_entries()` to automatically trigger a snapshot when `last_applied - snapshot_last_index > threshold`. Can be disabled by setting threshold to 0.

**Recovery on Startup**

Updated `RaftCore::new()` to load any existing snapshot from storage and restore the state machine before applying any remaining log entries. This ensures state is fully recovered after a restart.

**Prompt:** "log file itself is not compacted" and "compaction is not triggered the second time"

**Fixing Log Compaction Bugs**

Discovered two bugs in the log compaction implementation. First, `truncate_log()` had inverted filter logic - it was keeping entries with `index < from_index` instead of `index >= from_index`. Second, there was a fundamental semantic confusion: log truncation for conflict resolution (keep older entries) vs. log compaction for snapshotting (keep newer entries) are opposite operations. Split into two distinct methods: `truncate_log()` removes entries >= index (for conflict resolution) and `compact_log()` removes entries < index (for snapshotting).

**Prompt:** "now replication is cycling"

**Fixing Replication After Snapshot**

After compaction, replication was stuck in an infinite loop. Root cause: `apply_entries()` used direct Vec indexing `(entry.index - 1)` without accounting for the snapshot offset. After snapshot at index S, `log[0]` contains entry S+1, so the correct formula is `(entry.index - snapshot_last_index - 1)`.

**Prompt:** "entries still not committing after snapshot"

**Fixing handle_append_entries_result**

Found another place using direct Vec indexing: `handle_append_entries_result()` used `self.log.get((entry_index - 1) as usize)` instead of the helper `get_log_entry()` that accounts for snapshot offset.

**Prompt:** "Failed handle_append_entries: 0 1" (prev_log_term=0 instead of 1)

**The Root Cause: replicate_to_peers**

After extensive debugging with logging on both sender and receiver sides, discovered the real cluster was failing but tests passed. The difference: tests use `send_heartbeat()` which was fixed, but real client requests use `replicate_to_peers()` which had the same direct Vec indexing bug. When `prev_log_index == snapshot_last_index`, we must use `snapshot_last_term` instead of trying to look up the entry in the now-empty log.

**Prompt:** "why tests didn't catch this bug?" and "let's write a test that catches this"

**Test Coverage Gap**

Explained why tests didn't catch the `replicate_to_peers()` bug: tests used `send_heartbeat()` for replication whereas the real cluster uses `replicate_to_peers()` for client commands. Added `test_replicate_to_peers_after_snapshot` that explicitly takes a snapshot and then calls `replicate_to_peers()` to verify correct `prev_log_term` calculation. The test was verified to catch the bug by temporarily reverting the fix.

All 173 tests now pass, including the new regression test.

**Fixing Log Compaction Bugs**

Discovered two bugs in the log compaction implementation. First, `truncate_log()` had inverted filter logic - it was keeping entries with `index < from_index` instead of `index >= from_index`. Second, there was a fundamental semantic confusion: log truncation for conflict resolution (keep older entries) vs. log compaction for snapshotting (keep newer entries) are opposite operations. Split into two distinct methods: `truncate_log()` removes entries >= index (for conflict resolution) and `compact_log()` removes entries < index (for snapshotting).

**Prompt:** "now replication is cycling"

**Fixing Replication After Snapshot**

After compaction, replication was stuck in an infinite loop. Root cause: `apply_entries()` used direct Vec indexing `(entry.index - 1)` without accounting for the snapshot offset. After snapshot at index S, `log[0]` contains entry S+1, so the correct formula is `(entry.index - snapshot_last_index - 1)`.

**Prompt:** "entries still not committing after snapshot"

**Fixing handle_append_entries_result**

Found another place using direct Vec indexing: `handle_append_entries_result()` used `self.log.get((entry_index - 1) as usize)` instead of the helper `get_log_entry()` that accounts for snapshot offset.

**Prompt:** "Failed handle_append_entries: 0 1" (prev_log_term=0 instead of 1)

**The Root Cause: replicate_to_peers**

After extensive debugging with logging on both sender and receiver sides, discovered the real cluster was failing but tests passed. The difference: tests use `send_heartbeat()` which was fixed, but real client requests use `replicate_to_peers()` which had the same direct Vec indexing bug. When `prev_log_index == snapshot_last_index`, we must use `snapshot_last_term` instead of trying to look up the entry in the now-empty log.

**Prompt:** "why tests didn't catch this bug?" and "let's write a test that catches this"

**Test Coverage Gap**

Explained why tests didn't catch the `replicate_to_peers()` bug: tests used `send_heartbeat()` for replication whereas the real cluster uses `replicate_to_peers()` for client commands. Added `test_replicate_to_peers_after_snapshot` that explicitly takes a snapshot and then calls `replicate_to_peers()` to verify correct `prev_log_term` calculation. The test was verified to catch the bug by temporarily reverting the fix.

All 173 tests now pass, including the new regression test.

---

## Day 13: HTTP Cluster Integration Tests

**Prompt:** "Implement the following plan: Real HTTP Concurrency Tests"

**Creating Integration Test Infrastructure**

Created a new integration test file `tests/http_cluster.rs` for testing actual HTTP servers in a 3-node cluster configuration. Built a `TestCluster` struct that manages node lifecycle including startup, shutdown, and leader discovery. Each `TestNode` holds an HTTP server address, a shutdown channel for graceful HTTP termination, and a `RaftHandle` for RaftServer shutdown.

**Test Cases Implemented**

Implemented 8 comprehensive integration tests:
1. `test_cluster_elects_leader` - Verifies exactly one leader is elected
2. `test_concurrent_client_commands` - 20 concurrent writes verify correct replication
3. `test_rapid_fire_http_requests` - 100 rapid-fire commands stress test
4. `test_concurrent_reads_and_writes` - Mixed read/write operations
5. `test_follower_redirect` - Verifies proper error with leader_hint when submitting to follower
6. `test_leader_failover` - Verifies new leader election after killing the current leader
7. `test_concurrent_commands_during_leader_failover` - Commands continue working through failover
8. `test_cluster_consistency` - All nodes have identical state after operations

**Bug Fixes Discovered During Testing**

**1. Election Timeout Not Firing**
After killing a leader, followers never started elections. Diagnosed the issue: the leader's RaftServer was still running and sending heartbeats to followers even though its HTTP server was stopped. Added a `shutdown()` method to `RaftHandle` that sends a shutdown signal to the RaftServer loop, ensuring both HTTP and Raft servers stop together.

**2. MissedTickBehavior Causing Starvation**
Initially suspected `tokio::time::Interval`'s default `MissedTickBehavior::Burst` was starving the election timeout branch, but this wasn't the root cause. Added `set_missed_tick_behavior(MissedTickBehavior::Delay)` anyway for correctness.

**3. Leader Election Timeout Busy Loop**
After a node became leader, its election timeout kept firing (deadline was always in the past) causing a busy loop. Fixed by updating `last_heartbeat` when: (a) a node becomes leader via `become_leader()`, and (b) the leader sends heartbeats in the RaftServer loop.

**Architecture Improvements**

- Added `shutdown()` method to `RaftHandle` for graceful RaftServer termination
- Added shutdown channel (`mpsc`) to `RaftServer` struct with corresponding select! branch
- Updated `become_leader()` to reset `last_heartbeat` to prevent election timeout on leaders
- RaftServer now updates `last_heartbeat` when sending heartbeats as leader

All 181 tests pass (173 unit tests + 8 integration tests).

**Prompt:** "I want one more integration/concurrency test. big number of concurrent write requests for one key in kv state machine with different values. verify that all cluster nodes have the same state machine state which is the last command from the log and also logs are consistent across cluster"

**Concurrent Writes to Same Key Test**

Added a 9th integration test `test_concurrent_writes_to_same_key` that tests Raft's linearizability guarantees. The test submits 50 concurrent writes to the same key (`contested_key`) with different values (`writer_0` through `writer_49`). All writes are sent simultaneously to the leader.

Key verifications:
1. All 50 writes succeed (Raft serializes concurrent commands)
2. For each node, the state machine value for `contested_key` matches the value from the last log entry
3. All nodes have identical logs (same entries in same order)
4. All state machines have identical state across the cluster
5. Log length is exactly 51 (1 NOOP + 50 writes)

This test validates that even under high concurrent write contention to the same key, Raft maintains consistency: the final state is determined by the order in which commands were serialized into the log.

Added `shared_core: SharedCore` field to `TestNode` to enable direct log inspection without introducing new HTTP endpoints.

All 182 tests pass (173 unit tests + 9 integration tests).

---

## Day 13: ReadIndex Linearizable Reads

**Prompt:** "Implement the ReadIndex algorithm for linearizable reads"

**The Problem with Non-Linearizable Reads**

Previously, reads bypassed Raft entirely - `GET /client/get/{key}` read directly from the `SharedKvStore`. This could return stale data from a partitioned leader that hasn't realized it's no longer the leader.

**ReadIndex Algorithm Implementation**

Implemented the ReadIndex algorithm for linearizable reads:
1. Client sends read request to leader
2. Leader records current `commit_index` as `read_index`
3. Leader sends heartbeats to confirm it's still leader (majority must respond)
4. Once confirmed, leader waits until `last_applied >= read_index`
5. Leader reads from state machine and returns result

**Changes Made**

1. **RaftError::ReadTimeout** - New error variant for when leadership confirmation or apply times out

2. **Command::ReadIndex** - New command variant in the server's command enum, allowing clients to request a read index

3. **RaftHandle::read_index()** - Public method to request a linearizable read index

4. **send_heartbeat() return type** - Changed from `-> bool` to `-> (bool, usize)` to return both whether still leader and how many peers responded successfully (needed for majority confirmation)

5. **handle_read_index()** - Core implementation that:
   - Checks if leader and gets `read_index = commit_index`
   - Sends heartbeat to confirm leadership (requires majority response)
   - Polls until `last_applied >= read_index` (with timeout)
   - Returns the `last_applied` index

6. **HTTP Endpoint** - Added `GET /client/read/:key` that:
   - Calls `read_index()` to confirm leadership
   - Reads from KV store after confirmation
   - Returns `ReadResponse` with key, value, and read_index

7. **Updated Tests** - Replaced non-linearizable `get_kv()` and `get_value()` helpers with `linearizable_read()` in integration tests

**New Tests**

- `test_linearizable_read_not_leader` - Returns NotLeader error when sent to follower
- `test_linearizable_read_as_leader` - Succeeds after writing a value
- `test_linearizable_read_missing_key` - Returns None for non-existent keys
- `test_linearizable_read_after_writes` - Verifies reading latest value after multiple writes

**Lower-Level Unit Tests (in raft_server.rs)**

- `test_read_index_not_leader` - Returns NotLeader error when node is not leader
- `test_read_index_as_leader` - Succeeds for leader with majority responding
- `test_read_index_returns_last_applied` - Verifies returned index matches last_applied
- `test_read_index_timeout_no_majority` - Returns ReadTimeout when peers don't respond
- `test_read_index_leader_steps_down` - Returns NotLeader if discovers higher term during heartbeat
- `test_read_index_with_pending_entries` - Verifies read_index waits for entries to be applied

**API Changes**

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/client/submit` | POST | Write command (existing) |
| `/client/read/:key` | GET | **Linearizable read (new)** |
| `/client/leader` | GET | Get leader info (existing) |
| `/client/status` | GET | Get node status (existing) |

Removed the old non-linearizable endpoints (`/client/get/:key`, `/client/kv`).

All 192 tests pass (179 unit tests + 13 integration tests).

---

## Next Up

- Potential future work: Dynamic cluster membership, cluster configuration changes

---

## Notes

This log documents the learning journey and can be expanded into blog posts. Each entry captures what was accomplished and learned during the vibe coding session.

