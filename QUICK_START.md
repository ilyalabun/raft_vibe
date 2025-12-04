# Quick Start Guide 🚀

Welcome! This guide will get you up and running in 5 minutes.

## Step 1: Verify Rust Installation

```bash
rustc --version
cargo --version
```

If these don't work, install Rust:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Step 2: Run the Demo

```bash
cargo run
```

You should see a demo of a 3-node Raft cluster:
- Node 1 starts an election
- Gets votes from other nodes
- Becomes leader
- Replicates a log entry to followers

## Step 3: Run Examples

```bash
# See Rust basics in action
cargo run --example basics
```

## Step 4: Run Tests

```bash
cargo test
```

You should see 4 tests pass!

## Step 5: Explore the Code

1. **Read the basics**: Open `rust_basics.md`
2. **See the implementation**: Open `src/raft.rs`
3. **Understand the demo**: Open `src/main.rs`

## Step 6: Start Learning!

### Option A: Learn Rust First
1. Read `rust_basics.md` thoroughly
2. Run `cargo run --example basics`
3. Modify the examples and see what happens
4. Then come back to the Raft code

### Option B: Learn Raft First
1. Read the [Raft paper](https://raft.github.io/raft.pdf) (or watch videos)
2. Run `cargo run` to see it in action
3. Read `src/raft.rs` with the paper in hand
4. Ask questions about anything unclear

### Option C: Learn Both Together (Recommended!)
1. Read `rust_basics.md` sections 1-6
2. Run `cargo run` to see the demo
3. Read `src/raft.rs` and ask questions
4. Experiment by modifying code

## Common First Questions

**Q: What does `&mut self` mean?**  
A: It's a mutable reference to `self`. Read `rust_basics.md` section 2!

**Q: Why do we need `Option<u64>` for `voted_for`?**  
A: It can be `None` (no vote) or `Some(id)` (voted for this ID). Rust doesn't have `null`!

**Q: What's the difference between `Follower`, `Candidate`, and `Leader`?**  
A: These are the three states in Raft. Read the README or ask for an explanation!

**Q: How do I modify the code?**  
A: Just edit the files! Try changing the number of nodes or adding print statements.

## What to Do Next

1. **Ask questions**: "I don't understand X"
2. **Experiment**: Modify `main.rs` to add more nodes
3. **Read**: Go through `src/raft.rs` line by line
4. **Test**: Add your own test cases
5. **Extend**: "Can we add X feature?"

## Getting Help

- Read `VIBE_CODING_GUIDE.md` for tips on working with AI
- Ask questions in natural language
- Request explanations: "Explain how X works"
- Ask for examples: "Show me how to do Y"

## Project Structure

```
raft_vibe/
├── src/
│   ├── main.rs          # Demo program
│   └── raft.rs          # Raft implementation
├── examples/
│   └── basics.rs        # Rust basics examples
├── README.md            # Overview
├── rust_basics.md       # Rust tutorial
├── VIBE_CODING_GUIDE.md # How to work with AI
└── QUICK_START.md       # This file
```

## Ready to Code?

Just start asking questions or making changes! The AI assistant is here to help. 🎉

Try this: "Can you explain how the election process works step by step?"

