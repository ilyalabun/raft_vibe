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

---

## Next Up

- **Day 8: State Machine** - Implement a pluggable state machine abstraction and a simple key-value store

---

## Notes

This log documents the learning journey and can be expanded into blog posts. Each entry captures what was accomplished and learned during the vibe coding session.

