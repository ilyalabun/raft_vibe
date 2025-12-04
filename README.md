# Learning Raft in Rust 🦀

Welcome! This project is designed to teach you both **Rust** and the **Raft consensus protocol** from scratch.

## 📚 Learning Path

### Phase 1: Rust Basics (Start Here!)
1. Read `rust_basics.md` - Essential Rust concepts you'll need
2. Run examples: `cargo run --example basics`

### Phase 2: Understanding Raft
1. Read the Raft paper: https://raft.github.io/raft.pdf (or watch videos)
2. Key concepts:
   - **Leader Election**: How nodes elect a leader
   - **Log Replication**: How the leader replicates logs to followers
   - **Safety**: Ensuring consistency across all nodes

### Phase 3: Implementation
We'll build Raft step by step:
1. **Data Structures** - State, LogEntry, RPC messages
2. **State Machine** - Follower, Candidate, Leader states
3. **Election Logic** - RequestVote RPC
4. **Log Replication** - AppendEntries RPC

## 🏗️ Project Structure

```
raft_vibe/
├── src/
│   ├── main.rs              # Main entry point
│   ├── raft.rs              # Core Raft implementation
│   ├── state.rs             # Raft state machine
│   └── rpc.rs               # RPC message types
├── examples/
│   └── basics.rs            # Rust basics examples
├── rust_basics.md           # Rust tutorial
└── README.md                # This file
```

## 🚀 Getting Started

### Prerequisites
- Install Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Verify: `rustc --version` and `cargo --version`

### Running the Project
```bash
# Run the main program
cargo run

# Run with examples
cargo run --example basics

# Run tests
cargo test

# Build for release
cargo build --release
```

## 🎯 What is Raft?

Raft is a consensus algorithm designed to be understandable. It's used in distributed systems to ensure all nodes agree on the same state.

**Key Components:**
- **Leader**: One node that handles all client requests
- **Followers**: Other nodes that replicate the leader's log
- **Elections**: When the leader fails, followers elect a new leader
- **Log**: Ordered sequence of commands that all nodes must agree on

## 📖 Resources

- [Raft Paper](https://raft.github.io/raft.pdf)
- [Raft Visualization](https://raft.github.io/)
- [Rust Book](https://doc.rust-lang.org/book/)
- [Rust by Example](https://doc.rust-lang.org/rust-by-example/)

## 💡 Vibe Coding Tips

When working with an AI assistant:
1. **Ask questions** - Don't hesitate to ask "why" or "how does this work?"
2. **Request explanations** - If code is unclear, ask for clarification
3. **Iterate** - We can refine code together
4. **Learn incrementally** - We'll build complexity gradually

Let's start coding! 🚀

