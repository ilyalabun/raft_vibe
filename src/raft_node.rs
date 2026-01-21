//! RaftNode - High-level Raft node with consensus logic

use std::sync::Arc;
use tokio::sync::Mutex;

use crate::raft_core::{AppendEntriesArgs, RaftCore, RaftState, RequestVoteArgs};
use crate::transport::Transport;

/// Shared reference to RaftCore
pub type SharedCore = Arc<Mutex<RaftCore>>;

/// High-level Raft node that handles consensus operations
pub struct RaftNode<T: Transport> {
    core: SharedCore,
    transport: T,
}

impl<T: Transport> RaftNode<T> {
    /// Create a new RaftNode
    pub fn new(core: RaftCore, transport: T) -> Self {
        Self {
            core: Arc::new(Mutex::new(core)),
            transport,
        }
    }

    /// Get a shared reference to the core (for incoming RPC handling)
    pub fn shared_core(&self) -> SharedCore {
        self.core.clone()
    }

    /// Start an election
    pub async fn start_election(&self) {
        let mut core = self.core.lock().await;
        core.start_election();
    }

    /// Request votes from all peers (sends requests concurrently)
    /// Returns true if became leader
    pub async fn request_votes(&self) -> bool {
        let (args, peers) = {
            let core = self.core.lock().await;
            let args = RequestVoteArgs {
                term: core.current_term,
                candidate_id: core.id,
                last_log_index: core.last_log_index(),
                last_log_term: core.last_log_term(),
            };
            (args, core.peers.clone())
        };

        // Send all vote requests concurrently
        let futures: Vec<_> = peers
            .iter()
            .map(|&peer_id| {
                let args = args.clone();
                let transport = &self.transport;
                async move { (peer_id, transport.request_vote(peer_id, args).await) }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        // Process results
        for (peer_id, result) in results {
            if let Ok(result) = result {
                let mut core = self.core.lock().await;
                if core.handle_request_vote_result(peer_id, &result) {
                    return true; // Became leader
                }
            }
        }

        false
    }

    /// Replicate log entries to all peers (sends requests concurrently)
    pub async fn replicate_to_peers(&self, entry_index: u64) {
        let requests_to_send = {
            let core = self.core.lock().await;

            let mut requests_to_send = Vec::new();
            for &peer_id in &core.peers {
                let next_idx = core.next_index.get(&peer_id).copied().unwrap_or(1);
                let prev_log_index = next_idx - 1;
                let prev_log_term = if prev_log_index == 0 {
                    0
                } else {
                    core.log
                        .get((prev_log_index - 1) as usize)
                        .map(|e| e.term)
                        .unwrap_or(0)
                };

                // Get entries to send
                let entries: Vec<_> = core
                    .log
                    .iter()
                    .filter(|e| e.index >= next_idx && e.index <= entry_index)
                    .cloned()
                    .collect();

                let args = AppendEntriesArgs {
                    term: core.current_term,
                    leader_id: core.id,
                    prev_log_index,
                    prev_log_term,
                    entries,
                    leader_commit: core.commit_index,
                };
                requests_to_send.push((peer_id, args));
            }
            requests_to_send
        };

        // Send to all peers concurrently (lock released)
        let futures: Vec<_> = requests_to_send
            .into_iter()
            .map(|(peer_id, args)| {
                let transport = &self.transport;
                async move { (peer_id, transport.append_entries(peer_id, args).await) }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        // Process results
        for (peer_id, result) in results {
            if let Ok(result) = result {
                let mut core = self.core.lock().await;
                core.handle_append_entries_result(peer_id, entry_index, &result);
            }
        }
    }

    /// Get current state
    pub async fn state(&self) -> RaftState {
        self.core.lock().await.state
    }

    /// Get commit index
    pub async fn commit_index(&self) -> u64 {
        self.core.lock().await.commit_index
    }

    /// Send heartbeat to all peers
    /// Heartbeats in Raft are AppendEntries RPCs that also include any entries
    /// the follower might be missing (for catch-up). Returns true if still leader.
    pub async fn send_heartbeat(&self) -> bool {
        let requests_to_send = {
            let core = self.core.lock().await;

            // Only leaders send heartbeats
            if core.state != RaftState::Leader {
                return false;
            }

            let mut requests_to_send = Vec::new();
            for &peer_id in &core.peers {
                let next_idx = core.next_index.get(&peer_id).copied().unwrap_or(1);
                let prev_log_index = next_idx - 1;
                let prev_log_term = if prev_log_index == 0 {
                    0
                } else {
                    core.log
                        .get((prev_log_index - 1) as usize)
                        .map(|e| e.term)
                        .unwrap_or(0)
                };

                // Include entries from next_idx onwards for catch-up
                let entries: Vec<_> = core
                    .log
                    .iter()
                    .filter(|e| e.index >= next_idx)
                    .cloned()
                    .collect();

                // Track the last entry index we're sending (for result handling)
                let last_entry_index = entries.last().map(|e| e.index).unwrap_or(0);

                let args = AppendEntriesArgs {
                    term: core.current_term,
                    leader_id: core.id,
                    prev_log_index,
                    prev_log_term,
                    entries,
                    leader_commit: core.commit_index,
                };
                requests_to_send.push((peer_id, args, last_entry_index));
            }
            requests_to_send
        };

        // Send to all peers concurrently (lock released)
        let futures: Vec<_> = requests_to_send
            .into_iter()
            .map(|(peer_id, args, last_entry_index)| {
                let transport = &self.transport;
                async move { (peer_id, transport.append_entries(peer_id, args).await, last_entry_index) }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        // Process results
        for (peer_id, result, last_entry_index) in results {
            if let Ok(result) = result {
                let mut core = self.core.lock().await;
                core.handle_append_entries_result(peer_id, last_entry_index, &result);
            }
        }

        // Return whether we're still leader
        self.core.lock().await.state == RaftState::Leader
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_memory::MemoryStorage;
    use crate::transport_inmemory::create_cluster;

    /// Helper to create RaftCore with MemoryStorage for tests
    fn new_test_core(id: u64, peers: Vec<u64>) -> RaftCore {
        RaftCore::new(
            id,
            peers,
            Box::new(MemoryStorage::new()),
            Box::new(crate::state_machine::TestStateMachine::new()),
        )
    }

    #[tokio::test]
    async fn test_election() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        // Start election
        node1.start_election().await;

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Process vote requests concurrently
        let (became_leader, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        assert!(became_leader);
        assert_eq!(node1.state().await, RaftState::Leader);
    }

    #[tokio::test]
    async fn test_replication() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election first
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Submit a command
        let entry_index = {
            let mut core = node1.core.lock().await;
            let entry = core.append_log_entry("SET x=1".to_string()).unwrap();
            entry.index
        };

        // Replicate to peers
        let (_, _, _) = tokio::join!(
            node1.replicate_to_peers(entry_index),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Entry should be committed
        assert_eq!(node1.commit_index().await, entry_index);
        assert_eq!(shared2.lock().await.log.len(), 1);
        assert_eq!(shared3.lock().await.log.len(), 1);
    }

    #[tokio::test]
    async fn test_heartbeat() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election first
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Send heartbeat
        let (still_leader, _, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Should still be leader after heartbeat
        assert!(still_leader);
        assert_eq!(node1.state().await, RaftState::Leader);

        // Followers should remain followers with updated term
        assert_eq!(shared2.lock().await.state, RaftState::Follower);
        assert_eq!(shared3.lock().await.state, RaftState::Follower);
        assert_eq!(shared2.lock().await.current_term, 1);
        assert_eq!(shared3.lock().await.current_term, 1);
    }

    #[tokio::test]
    async fn test_heartbeat_catches_up_followers() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election first
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Add entries to leader's log without replicating
        {
            let mut core = node1.core.lock().await;
            core.append_log_entry("SET x=1".to_string()).unwrap();
            core.append_log_entry("SET y=2".to_string()).unwrap();
        }

        // Verify followers don't have entries yet
        assert_eq!(shared2.lock().await.log.len(), 0);
        assert_eq!(shared3.lock().await.log.len(), 0);

        // Send heartbeat - should replicate missing entries
        let (_, _, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Followers should now have the entries
        assert_eq!(shared2.lock().await.log.len(), 2);
        assert_eq!(shared3.lock().await.log.len(), 2);
        assert_eq!(shared2.lock().await.log[0].command, "SET x=1");
        assert_eq!(shared2.lock().await.log[1].command, "SET y=2");
    }

    #[tokio::test]
    async fn test_multiple_entries_replicated() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Submit multiple commands
        let entry3_index = {
            let mut core = node1.core.lock().await;
            core.append_log_entry("CMD 1".to_string()).unwrap();
            core.append_log_entry("CMD 2".to_string()).unwrap();
            core.append_log_entry("CMD 3".to_string()).unwrap().index
        };

        // Replicate all entries at once
        let (_, _, _) = tokio::join!(
            node1.replicate_to_peers(entry3_index),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // All entries should be replicated and committed
        assert_eq!(node1.commit_index().await, entry3_index);
        assert_eq!(shared2.lock().await.log.len(), 3);
        assert_eq!(shared3.lock().await.log.len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn test_election_with_one_peer_timeout() {
        use std::time::Duration;
        use crate::transport_inmemory::create_cluster_with_timeout;

        // In a 3-node cluster, need 2 votes (self + 1 peer)
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        // Node 3 won't respond (simulating crash/partition)

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));

        let mut handle2 = handles.remove(&2).unwrap();

        node1.start_election().await;

        // Only node 2 responds, node 3 times out
        let (became_leader, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
        );

        // Should still become leader with self + node2 = majority
        assert!(became_leader);
        assert_eq!(node1.state().await, RaftState::Leader);
    }

    #[tokio::test(start_paused = true)]
    async fn test_replication_with_one_peer_timeout() {
        use std::time::Duration;
        use crate::transport_inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        // Node 3 won't respond

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));

        let mut handle2 = handles.remove(&2).unwrap();

        // Win election first
        node1.start_election().await;
        let (_, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Submit a command
        let entry_index = {
            let mut core = node1.core.lock().await;
            core.append_log_entry("SET x=1".to_string()).unwrap().index
        };

        // Replicate - only node 2 responds, node 3 times out
        let (_, _) = tokio::join!(
            node1.replicate_to_peers(entry_index),
            handle2.process_one_shared(&shared2),
        );

        // Entry should be committed (leader + node2 = majority)
        assert_eq!(node1.commit_index().await, entry_index);
        assert_eq!(shared2.lock().await.log.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_with_timeout() {
        use std::time::Duration;
        use crate::transport_inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let node1 = RaftNode::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Send heartbeat - node 3 doesn't respond (times out)
        let (still_leader, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            // handle3 not processed - times out
        );

        // Should still be leader
        assert!(still_leader);
        assert_eq!(node1.state().await, RaftState::Leader);
    }

    #[tokio::test(start_paused = true)]
    async fn test_all_peers_timeout_election_fails() {
        use std::time::Duration;
        use crate::transport_inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, _handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();
        let node1 = RaftNode::new(core1, transport1);

        node1.start_election().await;

        // Neither peer responds - all timeout
        let became_leader = node1.request_votes().await;

        // Should not become leader (only has self-vote, need 2)
        assert!(!became_leader);
        assert_eq!(node1.state().await, RaftState::Candidate);
    }
}
