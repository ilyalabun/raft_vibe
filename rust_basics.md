# Rust Basics for Raft Implementation

This guide covers essential Rust concepts you'll need for implementing Raft.

## 1. Variables and Mutability

```rust
// Immutable by default
let x = 5;
// x = 6; // ERROR! Can't modify immutable variable

// Mutable variables
let mut y = 5;
y = 6; // OK!
```

## 2. Ownership and Borrowing

Rust's unique feature - memory safety without garbage collection!

```rust
// Ownership: Each value has ONE owner
let s1 = String::from("hello");
let s2 = s1; // s1 is moved to s2, s1 is no longer valid
// println!("{}", s1); // ERROR! s1 was moved

// Borrowing: References allow access without taking ownership
let s3 = String::from("world");
let len = calculate_length(&s3); // & = borrow (reference)
println!("{}", s3); // OK! s3 still owns the value

fn calculate_length(s: &String) -> usize {
    s.len()
}

// Mutable references
let mut s4 = String::from("hello");
change(&mut s4); // &mut = mutable borrow

fn change(s: &mut String) {
    s.push_str(", world");
}
```

## 3. Structs (Data Structures)

```rust
// Define a struct
struct Node {
    id: u64,
    state: String,
    term: u64,
}

// Create an instance
let node = Node {
    id: 1,
    state: String::from("follower"),
    term: 0,
};

// Mutable struct
let mut node = Node { id: 1, state: String::from("follower"), term: 0 };
node.term = 1; // Can modify if struct is mutable
```

## 4. Enums (Tagged Unions)

Perfect for Raft states!

```rust
enum RaftState {
    Follower,
    Candidate,
    Leader,
}

// Enums with data
enum Message {
    RequestVote { term: u64, candidate_id: u64 },
    AppendEntries { term: u64, entries: Vec<LogEntry> },
    VoteResponse { term: u64, vote_granted: bool },
}
```

## 5. Pattern Matching (match)

```rust
let state = RaftState::Leader;

match state {
    RaftState::Follower => println!("I'm a follower"),
    RaftState::Candidate => println!("I'm a candidate"),
    RaftState::Leader => println!("I'm the leader!"),
}

// Match with data
match message {
    Message::RequestVote { term, candidate_id } => {
        println!("Vote request from {} in term {}", candidate_id, term);
    }
    Message::AppendEntries { term, entries } => {
        println!("Append entries in term {}", term);
    }
    _ => println!("Other message"),
}
```

## 6. Vectors (Dynamic Arrays)

```rust
// Create a vector
let mut log: Vec<LogEntry> = Vec::new();

// Or with initial values
let mut log = vec![entry1, entry2];

// Add elements
log.push(entry3);

// Access elements
let first = &log[0]; // Reference
let first = log.get(0); // Returns Option<&LogEntry>

// Iterate
for entry in &log {
    println!("{:?}", entry);
}
```

## 7. Options and Results

Rust doesn't have `null` - it uses `Option` and `Result`!

```rust
// Option<T> = Some(T) or None
fn find_leader(nodes: &[Node]) -> Option<&Node> {
    for node in nodes {
        if node.state == "leader" {
            return Some(node);
        }
    }
    None
}

// Usage
match find_leader(&nodes) {
    Some(leader) => println!("Found leader: {}", leader.id),
    None => println!("No leader found"),
}

// Result<T, E> = Ok(T) or Err(E)
fn parse_term(s: &str) -> Result<u64, String> {
    match s.parse() {
        Ok(n) => Ok(n),
        Err(_) => Err(format!("Invalid term: {}", s)),
    }
}
```

## 8. Methods and impl Blocks

```rust
struct RaftNode {
    id: u64,
    term: u64,
    state: RaftState,
}

impl RaftNode {
    // Associated function (like static method)
    fn new(id: u64) -> Self {
        RaftNode {
            id,
            term: 0,
            state: RaftState::Follower,
        }
    }

    // Method (takes &self)
    fn get_term(&self) -> u64 {
        self.term
    }

    // Mutable method (takes &mut self)
    fn increment_term(&mut self) {
        self.term += 1;
    }

    // Takes ownership (takes self)
    fn into_state(self) -> RaftState {
        self.state
    }
}
```

## 9. Traits (Interfaces)

```rust
// Define a trait
trait RaftNode {
    fn get_term(&self) -> u64;
    fn become_leader(&mut self);
}

// Implement the trait
impl RaftNode for MyNode {
    fn get_term(&self) -> u64 {
        self.term
    }
    
    fn become_leader(&mut self) {
        self.state = RaftState::Leader;
    }
}
```

## 10. Closures (Lambdas)

```rust
// Simple closure
let add_one = |x| x + 1;
println!("{}", add_one(5)); // 6

// Closure with types
let find_node = |nodes: &[Node], id: u64| -> Option<&Node> {
    nodes.iter().find(|n| n.id == id)
};

// Using closures with iterators
let numbers = vec![1, 2, 3, 4, 5];
let doubled: Vec<i32> = numbers.iter().map(|x| x * 2).collect();
```

## 11. Error Handling

```rust
// Using Result
fn process_vote(term: u64) -> Result<bool, String> {
    if term < self.current_term {
        return Err("Term is too old".to_string());
    }
    Ok(true)
}

// Using ? operator (propagates errors)
fn handle_vote(term: u64) -> Result<bool, String> {
    let valid = process_vote(term)?; // If Err, returns early
    Ok(valid)
}

// Using unwrap (panics on error - use carefully!)
let result = process_vote(5).unwrap();
```

## 12. Common Patterns for Raft

### Clone and Copy
```rust
#[derive(Clone, Copy, Debug)]
struct LogEntry {
    term: u64,
    index: u64,
    data: u64, // Simple data type
}
```

### Debug Printing
```rust
#[derive(Debug)]
struct Node {
    id: u64,
    term: u64,
}

let node = Node { id: 1, term: 5 };
println!("{:?}", node); // Debug format
println!("{:#?}", node); // Pretty debug format
```

### Default Values
```rust
#[derive(Default)]
struct RaftState {
    term: u64,
    voted_for: Option<u64>,
}

let state = RaftState::default(); // term: 0, voted_for: None
```

## Key Takeaways for Raft

1. **Enums** for states (Follower, Candidate, Leader)
2. **Structs** for nodes, log entries, RPC messages
3. **Options** for nullable values (voted_for, leader_id)
4. **Results** for error handling (network failures, invalid terms)
5. **Vectors** for log entries
6. **Match** for state transitions and message handling

## Next Steps

Now that you understand the basics, let's look at the actual Raft implementation in `src/raft.rs`!

