// Rust Basics Examples
// Run with: cargo run --example basics

fn main() {
    println!("=== Rust Basics Examples ===\n");

    // 1. Variables and Mutability
    println!("1. Variables:");
    let immutable = 5;
    let mut mutable = 5;
    mutable = 10;
    println!("   Immutable: {}, Mutable: {}\n", immutable, mutable);

    // 2. Structs
    println!("2. Structs:");
    #[derive(Debug)]
    struct Node {
        id: u64,
        term: u64,
    }
    
    let node = Node { id: 1, term: 5 };
    println!("   Node: {:?}\n", node);

    // 3. Enums
    println!("3. Enums:");
    #[derive(Debug)]
    enum State {
        Follower,
        Candidate,
        Leader,
    }
    
    let state = State::Leader;
    match state {
        State::Follower => println!("   State: Follower"),
        State::Candidate => println!("   State: Candidate"),
        State::Leader => println!("   State: Leader"),
    }
    println!();

    // 4. Vectors
    println!("4. Vectors:");
    let mut log = vec![1, 2, 3];
    log.push(4);
    println!("   Log: {:?}\n", log);

    // 5. Options
    println!("5. Options:");
    let maybe_leader: Option<u64> = Some(1);
    match maybe_leader {
        Some(id) => println!("   Leader ID: {}", id),
        None => println!("   No leader"),
    }
    println!();

    // 6. Methods
    println!("6. Methods:");
    struct RaftNode {
        term: u64,
    }
    
    impl RaftNode {
        fn new() -> Self {
            RaftNode { term: 0 }
        }
        
        fn increment_term(&mut self) {
            self.term += 1;
        }
        
        fn get_term(&self) -> u64 {
            self.term
        }
    }
    
    let mut node = RaftNode::new();
    node.increment_term();
    println!("   Term: {}\n", node.get_term());

    println!("=== Examples Complete! ===");
    println!("Read rust_basics.md for detailed explanations.");
}

