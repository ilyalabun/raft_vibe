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

---

## Notes

This log documents the learning journey and can be expanded into blog posts. Each entry captures what was accomplished and learned during the vibe coding session.

