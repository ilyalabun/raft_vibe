//! Raft Consensus Algorithm - Learning Implementation
//! 
//! This is an educational implementation of the Raft consensus protocol.
//! Start by reading README.md and rust_basics.md!

mod raft;

use raft::{RaftNode, RequestVoteArgs, AppendEntriesArgs};

fn main() {
    println!("=== Raft Consensus Algorithm Demo ===\n");

    // Create three nodes in a cluster
    let mut node1 = RaftNode::new(1, vec![2, 3]);
    let mut node2 = RaftNode::new(2, vec![1, 3]);
    let mut node3 = RaftNode::new(3, vec![1, 2]);

    println!("Created 3-node cluster:");
    println!("  Node 1: {:?}", node1.state);
    println!("  Node 2: {:?}", node2.state);
    println!("  Node 3: {:?}", node3.state);
    println!();

    // Node 1 starts an election
    println!("Node 1 starts an election...");
    node1.start_election();
    println!("  Node 1 state: {:?}, term: {}", node1.state, node1.current_term);
    println!();

    // Node 1 requests votes from other nodes
    println!("Node 1 requests votes...");
    let vote_args = RequestVoteArgs {
        term: node1.current_term,
        candidate_id: node1.id,
        last_log_index: node1.last_log_index(),
        last_log_term: node1.last_log_term(),
    };

    let vote_result_2 = node2.handle_request_vote(vote_args.clone());
    let vote_result_3 = node3.handle_request_vote(vote_args.clone());

    println!("  Node 2 vote: {}", if vote_result_2.vote_granted { "GRANTED" } else { "DENIED" });
    println!("  Node 3 vote: {}", if vote_result_3.vote_granted { "GRANTED" } else { "DENIED" });

    // Node 1 received 2 votes (itself + one other) = majority in 3-node cluster
    if vote_result_2.vote_granted || vote_result_3.vote_granted {
        println!("\nNode 1 received majority of votes!");
        node1.become_leader();
        println!("  Node 1 is now: {:?}", node1.state);
    }
    println!();

    // Leader receives a client command
    println!("Leader (Node 1) receives client command: SET x=42");
    let entry = node1.append_log_entry("SET x=42".to_string());
    println!("  Added log entry: term={}, index={}, command={}", 
             entry.term, entry.index, entry.command);
    println!();

    // Leader replicates log to followers
    println!("Leader replicates log to followers...");
    
    // Replicate to Node 2
    let append_args_2 = AppendEntriesArgs {
        term: node1.current_term,
        leader_id: node1.id,
        prev_log_index: node2.last_log_index(),
        prev_log_term: node2.last_log_term(),
        entries: vec![entry.clone()],
        leader_commit: node1.commit_index,
    };
    let append_result_2 = node2.handle_append_entries(append_args_2);
    println!("  Node 2 replication: {}", if append_result_2.success { "SUCCESS" } else { "FAILED" });
    println!("  Node 2 log length: {}", node2.log.len());

    // Replicate to Node 3
    let append_args_3 = AppendEntriesArgs {
        term: node1.current_term,
        leader_id: node1.id,
        prev_log_index: node3.last_log_index(),
        prev_log_term: node3.last_log_term(),
        entries: vec![entry.clone()],
        leader_commit: node1.commit_index,
    };
    let append_result_3 = node3.handle_append_entries(append_args_3);
    println!("  Node 3 replication: {}", if append_result_3.success { "SUCCESS" } else { "FAILED" });
    println!("  Node 3 log length: {}", node3.log.len());
    println!();

    // Show final state
    println!("=== Final State ===");
    println!("Node 1 (Leader):");
    println!("  State: {:?}", node1.state);
    println!("  Term: {}", node1.current_term);
    println!("  Log entries: {}", node1.log.len());
    println!("  Commit index: {}", node1.commit_index);
    
    println!("\nNode 2 (Follower):");
    println!("  State: {:?}", node2.state);
    println!("  Term: {}", node2.current_term);
    println!("  Log entries: {}", node2.log.len());
    
    println!("\nNode 3 (Follower):");
    println!("  State: {:?}", node3.state);
    println!("  Term: {}", node3.current_term);
    println!("  Log entries: {}", node3.log.len());

    println!("\n=== Demo Complete! ===");
    println!("\nNext steps:");
    println!("1. Read rust_basics.md to understand Rust concepts");
    println!("2. Read src/raft.rs to see the implementation");
    println!("3. Run tests: cargo test");
    println!("4. Try modifying the code!");
}
