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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport_inmemory::create_cluster;

    #[tokio::test]
    async fn test_election() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = RaftCore::new(1, vec![2, 3]);
        let core2 = RaftCore::new(2, vec![1, 3]);
        let core3 = RaftCore::new(3, vec![1, 2]);

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

        let core1 = RaftCore::new(1, vec![2, 3]);
        let core2 = RaftCore::new(2, vec![1, 3]);
        let core3 = RaftCore::new(3, vec![1, 2]);

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
}
