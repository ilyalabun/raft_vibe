# Vibe Coding Guide 🤝

Welcome to **vibe coding** - a collaborative style of programming where you work alongside an AI assistant! This guide will help you get the most out of this experience.

## What is Vibe Coding?

Vibe coding is an interactive, conversational approach to programming where:
- You describe what you want to build or learn
- The AI helps you understand concepts and writes code
- You ask questions, experiment, and iterate together
- Learning happens naturally through doing

## How to Communicate Effectively

### ✅ Good Requests

**Be specific about what you want:**
- "Add a function to calculate the majority of votes"
- "Explain how the election timeout works in Raft"
- "Show me how to handle network failures"
- "I want to add logging to see what's happening"

**Ask for explanations:**
- "Why do we need to check `last_log_term` in RequestVote?"
- "What does `&mut self` mean?"
- "How does Rust's ownership work here?"

**Request incremental changes:**
- "Can we add error handling to the RPC calls?"
- "Let's make the election timeout configurable"
- "Add a test for the split-vote scenario"

### ❌ Less Effective Requests

**Too vague:**
- "Make it better" (better how?)
- "Fix the code" (what's wrong?)
- "Do Raft" (too broad)

**Too complex all at once:**
- "Build a complete distributed system with networking, persistence, and a web UI" (let's break this down!)

## Workflow Tips

### 1. Start Small, Build Up
```
✅ Good: "Let's add a simple test first"
❌ Less ideal: "Implement the entire Raft protocol with all edge cases"
```

### 2. Ask Questions as You Go
- "What does this line do?"
- "Why did you choose this approach?"
- "Is there a simpler way?"

### 3. Experiment and Iterate
- Try modifying code yourself
- Ask for help when stuck
- Request explanations when confused

### 4. Learn by Understanding
- Don't just copy code - understand it
- Ask "why" not just "how"
- Request examples and analogies

## Example Conversation Flow

**You:** "I want to learn Rust and Raft"

**AI:** *Creates project structure, explains basics*

**You:** "What's the difference between `&self` and `&mut self`?"

**AI:** *Explains borrowing and mutability*

**You:** "Can we add a test for when two nodes start elections at the same time?"

**AI:** *Adds test, explains split-vote scenario*

**You:** "I don't understand why we need `prev_log_index`. Can you explain?"

**AI:** *Explains log consistency and the AppendEntries protocol*

## Common Patterns

### Learning a New Concept
1. Ask: "What is X?"
2. Request: "Show me a simple example"
3. Experiment: "Can I modify it to do Y?"
4. Deepen: "How does this relate to Z?"

### Adding Features
1. Describe: "I want to add X"
2. Clarify: "Should it work like Y?"
3. Implement: *AI writes code*
4. Refine: "Can we make it handle edge case Z?"

### Debugging
1. Describe: "When I run X, I get error Y"
2. Share: *Paste error message or code*
3. Understand: "Why is this happening?"
4. Fix: *AI helps debug and fix*

## Making the Most of This Project

### Phase 1: Understanding (You are here!)
- ✅ Read `rust_basics.md`
- ✅ Run `cargo run` to see the demo
- ✅ Read through `src/raft.rs` with questions
- ✅ Run `cargo test` to see tests

### Phase 2: Experimenting
- Modify the demo in `main.rs`
- Add your own tests
- Try changing the election logic
- Add print statements to trace execution

### Phase 3: Extending
- Add network simulation
- Implement election timeouts
- Add persistence (save state to disk)
- Handle more edge cases

### Phase 4: Advanced
- Add actual networking
- Implement snapshotting
- Add membership changes
- Build a simple key-value store on top

## Questions to Ask

### About Rust
- "What's the difference between X and Y?"
- "Why does Rust do X this way?"
- "How do I handle X in Rust?"
- "What's the idiomatic way to do X?"

### About Raft
- "Why does Raft need X?"
- "What happens if X fails?"
- "How does Raft guarantee Y?"
- "What's the purpose of X in the algorithm?"

### About the Code
- "Why did you structure it this way?"
- "Can we refactor this to be clearer?"
- "Is there a better way to do this?"
- "What are the trade-offs here?"

## Remember

1. **There are no stupid questions** - Ask anything!
2. **Break things** - Experimenting is how you learn
3. **Ask for explanations** - Understanding > memorizing
4. **Iterate** - We can refine code together
5. **Have fun!** - Learning should be enjoyable

## Next Steps

1. Run the demo: `cargo run`
2. Read the code: Open `src/raft.rs`
3. Ask questions: "I don't understand X"
4. Experiment: Modify something and see what happens
5. Extend: "Can we add Y?"

Ready to start? Just ask! 🚀

