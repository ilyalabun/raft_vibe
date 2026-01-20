//! Raft Consensus Algorithm - Learning Implementation
//! 
//! This is an educational implementation of the Raft consensus protocol.
//! Start by reading README.md and rust_basics.md!

mod config;
mod raft_node;
mod raft_core;
mod raft_server;
mod storage;
mod storage_memory;
mod transport;
mod transport_inmemory;

use raft_core::{RaftCore, RequestVoteArgs, AppendEntriesArgs};
use storage_memory::MemoryStorage;

fn main() {
    println!("=== Raft Consensus Algorithm Demo ===\n");

    // Create three nodes in a cluster
    let mut node1 = RaftCore::new(1, vec![2, 3], Box::new(MemoryStorage::new()));
    let mut node2 = RaftCore::new(2, vec![1, 3], Box::new(MemoryStorage::new()));
    let mut node3 = RaftCore::new(3, vec![1, 2], Box::new(MemoryStorage::new()));

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

    let vote_result_2 = node2.handle_request_vote(&vote_args);
    let vote_result_3 = node3.handle_request_vote(&vote_args);

    println!("  Node 2 vote: {}", if vote_result_2.vote_granted { "GRANTED" } else { "DENIED" });
    println!("  Node 3 vote: {}", if vote_result_3.vote_granted { "GRANTED" } else { "DENIED" });

    // Handle vote results (encapsulates term update, vote tracking, and becoming leader)
    let became_leader_2 = node1.handle_request_vote_result(2, &vote_result_2);
    let became_leader_3 = node1.handle_request_vote_result(3, &vote_result_3);

    if became_leader_2 || became_leader_3 {
        println!("\nNode 1 received majority of votes!");
        println!("  Node 1 is now: {:?}", node1.state);
    }
    println!();

    // Leader receives a client command
    println!("Leader (Node 1) receives client command: SET x=42");
    let entry = node1.append_log_entry("SET x=42".to_string());
    let entry = match entry {
        Some(e) => {
            println!("  Added log entry: term={}, index={}, command={}", 
                    e.term, e.index, e.command);
            e
        }
        None => {
            println!("  ERROR: Not a leader, cannot append entry!");
            return;
        }
    };
    println!();

    // Follower tries to receive a client command (should be rejected)
    println!("Follower (Node 2) receives client command: SET x=43");
    let follower_entry = node2.append_log_entry("SET x=43".to_string());
    match follower_entry {
        Some(e) => println!("  Added log entry: term={}, index={}, command={}", 
                           e.term, e.index, e.command),
        None => println!("  REJECTED: Followers cannot accept client commands!"),
    }
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
    let append_output_2 = node2.handle_append_entries(&append_args_2);
    println!("  Node 2 replication: {}", if append_output_2.result.success { "SUCCESS" } else { "FAILED" });
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
    let append_output_3 = node3.handle_append_entries(&append_args_3);
    println!("  Node 3 replication: {}", if append_output_3.result.success { "SUCCESS" } else { "FAILED" });
    println!("  Node 3 log length: {}", node3.log.len());

    // Handle append entries results (encapsulates term update, replication tracking, and committing)
    let committed_2 = node1.handle_append_entries_result(2, entry.index, &append_output_2.result);
    let committed_3 = node1.handle_append_entries_result(3, entry.index, &append_output_3.result);

    // If entry was committed, send updated commit_index to followers via AppendEntries (heartbeat)
    if committed_2.is_some() || committed_3.is_some() {
        let committed_index = committed_2.or(committed_3).unwrap();
        println!("\nEntry committed (index: {})", committed_index);
        
        // Send updated commit_index to followers via AppendEntries (heartbeat)
        let heartbeat_2 = AppendEntriesArgs {
            term: node1.current_term,
            leader_id: node1.id,
            prev_log_index: node2.last_log_index(),
            prev_log_term: node2.last_log_term(),
            entries: vec![],
            leader_commit: node1.commit_index,
        };
        let heartbeat_3 = AppendEntriesArgs {
            term: node1.current_term,
            leader_id: node1.id,
            prev_log_index: node3.last_log_index(),
            prev_log_term: node3.last_log_term(),
            entries: vec![],
            leader_commit: node1.commit_index,
        };
        node2.handle_append_entries(&heartbeat_2);
        node3.handle_append_entries(&heartbeat_3);
    }
    println!();

    // Show final state
    println!("=== Final State ===");
    println!("Node 1 (Leader):");
    println!("  State: {:?}", node1.state);
    println!("  Term: {}", node1.current_term);
    println!("  Log entries: {}", node1.log.len());
    println!("  Commit index: {}", node1.commit_index);
    println!("  Last applied: {}", node1.last_applied);
    
    println!("\nNode 2 (Follower):");
    println!("  State: {:?}", node2.state);
    println!("  Term: {}", node2.current_term);
    println!("  Log entries: {}", node2.log.len());
    println!("  Last applied: {}", node2.last_applied);
    
    println!("\nNode 3 (Follower):");
    println!("  State: {:?}", node3.state);
    println!("  Term: {}", node3.current_term);
    println!("  Log entries: {}", node3.log.len());
    println!("  Last applied: {}", node3.last_applied);

    println!("\n=== Demo Complete! ===");
    println!("\nNext steps:");
    println!("1. Read rust_basics.md to understand Rust concepts");
    println!("2. Read src/raft.rs to see the implementation");
    println!("3. Run tests: cargo test");
    println!("4. Try modifying the code!");
}
