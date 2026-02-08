//! RaftNode - High-level Raft node with consensus logic

use std::sync::Arc;
use tokio::sync::Mutex;

use futures::stream::FuturesUnordered;
use futures::StreamExt;

use super::raft_core::{AppendEntriesArgs, InstallSnapshotArgs, RaftCore, RaftState, RequestVoteArgs};
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

        // Send all vote requests concurrently, process as they arrive
        let mut futures: FuturesUnordered<_> = peers
            .iter()
            .map(|&peer_id| {
                let args = args.clone();
                let transport = &self.transport;
                async move { (peer_id, transport.request_vote(peer_id, args).await) }
            })
            .collect();

        while let Some((peer_id, result)) = futures.next().await {
            if let Ok(result) = result {
                let mut core = self.core.lock().await;
                if core.handle_request_vote_result(peer_id, &result) {
                    return true; // Became leader, don't wait for remaining
                }
            }
        }

        false
    }

    /// Replicate log entries to all peers (sends requests concurrently)
    /// Replicate entry at entry_index to all peers
    /// Returns the state machine result for entry_index if it was committed and applied
    pub async fn replicate_to_peers(&self, entry_index: u64) -> Option<Result<String, String>> {
        let requests_to_send = {
            let core = self.core.lock().await;

            let mut requests_to_send = Vec::new();
            for &peer_id in &core.peers {
                let next_idx = core.next_index.get(&peer_id).copied().unwrap_or(1);
                let prev_log_index = next_idx - 1;
                let prev_log_term = if prev_log_index == core.snapshot_last_index {
                    // Previous entry is exactly at snapshot boundary
                    core.snapshot_last_term
                } else if prev_log_index == 0 {
                    0
                } else if prev_log_index > core.snapshot_last_index {
                    // Entry is in log - calculate position directly
                    let pos = (prev_log_index - core.snapshot_last_index - 1) as usize;
                    core.log.get(pos).map(|e| e.term).unwrap_or(0)
                } else {
                    // Entry is before snapshot - shouldn't happen in normal operation
                    0
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

        // Send to all peers concurrently, process as they arrive (lock released)
        let mut futures: FuturesUnordered<_> = requests_to_send
            .into_iter()
            .map(|(peer_id, args)| {
                let transport = &self.transport;
                async move { (peer_id, transport.append_entries(peer_id, args).await) }
            })
            .collect();

        // Process results as they arrive - return as soon as entry is committed
        let mut entry_result = None;
        while let Some((peer_id, result)) = futures.next().await {
            if let Ok(result) = result {
                let mut core = self.core.lock().await;
                let (_committed, apply_results) = core.handle_append_entries_result(peer_id, entry_index, &result);
                for (idx, res) in apply_results {
                    if idx == entry_index {
                        entry_result = Some(res);
                    }
                }
            }
            if entry_result.is_some() {
                break; // Entry committed, don't wait for remaining peers
            }
        }
        entry_result
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
    /// the follower might be missing (for catch-up). If a follower is too far
    /// behind (needs entries that have been compacted), sends InstallSnapshot instead.
    /// Returns (still_leader, success_count) - whether still leader and how many peers responded successfully.
    pub async fn send_heartbeat(&self) -> (bool, usize) {
        // Enum to hold different request types
        enum Request {
            AppendEntries(AppendEntriesArgs, u64), // args, last_entry_index
            InstallSnapshot(InstallSnapshotArgs),
        }

        let requests_to_send = {
            let core = self.core.lock().await;

            // Only leaders send heartbeats
            if core.state != RaftState::Leader {
                return (false, 0);
            }

            let mut requests_to_send = Vec::new();
            for &peer_id in &core.peers {
                let next_idx = core.next_index.get(&peer_id).copied().unwrap_or(1);

                // Check if peer needs snapshot (next_idx is in the compacted part of log)
                if next_idx <= core.snapshot_last_index {
                    // Peer needs snapshot - load and send it
                    if let Ok(Some(snapshot)) = core.load_snapshot() {
                        let args = InstallSnapshotArgs {
                            term: core.current_term,
                            leader_id: core.id,
                            last_included_index: snapshot.metadata.last_included_index,
                            last_included_term: snapshot.metadata.last_included_term,
                            data: snapshot.data,
                        };
                        requests_to_send.push((peer_id, Request::InstallSnapshot(args)));
                    }
                    // If snapshot load fails, skip this peer for now
                    continue;
                }

                // Peer is caught up enough - send regular AppendEntries
                let prev_log_index = next_idx - 1;
                let prev_log_term = if prev_log_index == core.snapshot_last_index {
                    // Previous entry is exactly at snapshot boundary
                    core.snapshot_last_term
                } else if prev_log_index == 0 {
                    0
                } else if prev_log_index > core.snapshot_last_index {
                    // Entry is in log - calculate position directly
                    let pos = (prev_log_index - core.snapshot_last_index - 1) as usize;
                    core.log.get(pos).map(|e| e.term).unwrap_or(0)
                } else {
                    // Entry is before snapshot - shouldn't happen in normal operation
                    0
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
                requests_to_send.push((peer_id, Request::AppendEntries(args, last_entry_index)));
            }
            requests_to_send
        };

        // Send to all peers concurrently (lock released)
        enum ResultType {
            AppendEntries(Result<crate::core::raft_core::AppendEntriesResult, crate::transport::TransportError>, u64),
            InstallSnapshot(Result<crate::core::raft_core::InstallSnapshotResult, crate::transport::TransportError>, u64), // last_included_index
        }

        let mut futures: FuturesUnordered<_> = requests_to_send
            .into_iter()
            .map(|(peer_id, request)| {
                let transport = &self.transport;
                async move {
                    match request {
                        Request::AppendEntries(args, last_entry_index) => {
                            let result = transport.append_entries(peer_id, args).await;
                            (peer_id, ResultType::AppendEntries(result, last_entry_index))
                        }
                        Request::InstallSnapshot(args) => {
                            let last_included_index = args.last_included_index;
                            let result = transport.install_snapshot(peer_id, args).await;
                            (peer_id, ResultType::InstallSnapshot(result, last_included_index))
                        }
                    }
                }
            })
            .collect();

        // Process all results (wait for every peer so we catch higher terms and replicate fully)
        let mut success_count = 0;
        while let Some((peer_id, result_type)) = futures.next().await {
            match result_type {
                ResultType::AppendEntries(result, last_entry_index) => {
                    if let Ok(append_result) = result {
                        let mut core = self.core.lock().await;
                        let _ = core.handle_append_entries_result(peer_id, last_entry_index, &append_result);
                        success_count += 1;
                    }
                }
                ResultType::InstallSnapshot(result, last_included_index) => {
                    use crate::core::raft_core::InstallSnapshotResult;
                    if let Ok(snapshot_result) = result {
                        match snapshot_result {
                            InstallSnapshotResult::Success { term: _ } => {
                                let mut core = self.core.lock().await;
                                core.next_index.insert(peer_id, last_included_index + 1);
                                core.match_index.insert(peer_id, last_included_index);
                                success_count += 1;
                            }
                            InstallSnapshotResult::Failed { term, reason: _ } => {
                                let mut core = self.core.lock().await;
                                if term > core.current_term {
                                    core.current_term = term;
                                    core.state = RaftState::Follower;
                                    core.voted_for = None;
                                }
                            }
                        }
                    }
                }
            }
        }

        let still_leader = self.core.lock().await.state == RaftState::Leader;
        (still_leader, success_count)
    }

    /// Confirm leadership by sending heartbeats to peers.
    /// Returns (still_leader, success_count) as soon as a majority responds,
    /// without waiting for slow/dead peers. Used by ReadIndex protocol.
    pub async fn confirm_leadership(&self) -> (bool, usize) {
        // Build lightweight heartbeats (empty AppendEntries)
        let requests = {
            let core = self.core.lock().await;
            if core.state != RaftState::Leader {
                return (false, 0);
            }
            let mut requests = Vec::new();
            for &peer_id in &core.peers {
                let next_idx = core.next_index.get(&peer_id).copied().unwrap_or(1);
                let prev_log_index = next_idx - 1;
                let prev_log_term = if prev_log_index == core.snapshot_last_index {
                    core.snapshot_last_term
                } else if prev_log_index == 0 {
                    0
                } else if prev_log_index > core.snapshot_last_index {
                    let pos = (prev_log_index - core.snapshot_last_index - 1) as usize;
                    core.log.get(pos).map(|e| e.term).unwrap_or(0)
                } else {
                    0
                };
                let args = AppendEntriesArgs {
                    term: core.current_term,
                    leader_id: core.id,
                    prev_log_index,
                    prev_log_term,
                    entries: vec![],
                    leader_commit: core.commit_index,
                };
                requests.push((peer_id, args));
            }
            requests
        };

        let num_peers = requests.len();
        let majority_needed = (num_peers + 1) / 2;

        let mut futures: FuturesUnordered<_> = requests
            .into_iter()
            .map(|(peer_id, args)| {
                let transport = &self.transport;
                async move { (peer_id, transport.append_entries(peer_id, args).await) }
            })
            .collect();

        let mut success_count = 0;
        while let Some((_peer_id, result)) = futures.next().await {
            if let Ok(result) = result {
                let mut core = self.core.lock().await;
                if result.term > core.current_term {
                    core.current_term = result.term;
                    core.state = RaftState::Follower;
                    core.voted_for = None;
                    return (false, success_count);
                }
                if result.success {
                    success_count += 1;
                }
            }
            if success_count >= majority_needed {
                break;
            }
        }

        let still_leader = self.core.lock().await.state == RaftState::Leader;
        (still_leader, success_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory::MemoryStorage;
    use crate::transport::inmemory::create_cluster;

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

        // Win election first (become_leader appends NOOP)
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Submit a command (index 2, after NOOP at index 1)
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

        // Entry should be committed (NOOP + SET x=1)
        assert_eq!(node1.commit_index().await, entry_index);
        assert_eq!(shared2.lock().await.log.len(), 2); // NOOP + command
        assert_eq!(shared3.lock().await.log.len(), 2);
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
        let ((still_leader, _), _, _) = tokio::join!(
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
        use crate::core::raft_core::NOOP_COMMAND;

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

        // Win election first (become_leader appends NOOP)
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

        // Send heartbeat - should replicate missing entries (NOOP + 2 commands)
        let (_, _, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Followers should now have the entries (NOOP + SET x=1 + SET y=2)
        assert_eq!(shared2.lock().await.log.len(), 3);
        assert_eq!(shared3.lock().await.log.len(), 3);
        assert_eq!(shared2.lock().await.log[0].command, NOOP_COMMAND);
        assert_eq!(shared2.lock().await.log[1].command, "SET x=1");
        assert_eq!(shared2.lock().await.log[2].command, "SET y=2");
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

        // Win election (become_leader appends NOOP)
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Submit multiple commands (after NOOP at index 1)
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

        // All entries should be replicated and committed (NOOP + 3 commands)
        assert_eq!(node1.commit_index().await, entry3_index);
        assert_eq!(shared2.lock().await.log.len(), 4);
        assert_eq!(shared3.lock().await.log.len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn test_election_with_one_peer_timeout() {
        use std::time::Duration;
        use crate::transport::inmemory::create_cluster_with_timeout;

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
        use crate::transport::inmemory::create_cluster_with_timeout;

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

        // Win election first (become_leader appends NOOP)
        node1.start_election().await;
        let (_, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Submit a command (index 2, after NOOP)
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
        assert_eq!(shared2.lock().await.log.len(), 2); // NOOP + command
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_with_timeout() {
        use std::time::Duration;
        use crate::transport::inmemory::create_cluster_with_timeout;

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
        let ((still_leader, _), _) = tokio::join!(
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
        use crate::transport::inmemory::create_cluster_with_timeout;

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

    // ========== Snapshot Replication Tests ==========

    #[tokio::test]
    async fn test_heartbeat_sends_snapshot_to_lagging_follower() {
        use crate::state_machine::kv::KeyValueStore;

        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        // Create leader with KV store
        let mut leader = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        // Become leader and add some entries
        leader.start_election();
        leader.become_leader(); // Appends NOOP at index 1
        leader.append_log_entry("SET x 1".to_string()).unwrap(); // index 2
        leader.append_log_entry("SET y 2".to_string()).unwrap(); // index 3
        leader.append_log_entry("SET z 3".to_string()).unwrap(); // index 4

        // Apply all entries to state machine
        leader.commit_index = 4;
        leader.apply_committed_entries();

        // Take snapshot at last_applied (index 4)
        leader.take_snapshot().unwrap();

        // Verify snapshot was taken
        assert_eq!(leader.snapshot_last_index, 4);
        assert_eq!(leader.snapshot_last_term, 1);
        assert!(leader.log.is_empty()); // Log should be compacted

        // Create followers - node 2 is WAY behind (needs snapshot)
        let core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        let core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        let transport1 = transports.remove(&1).unwrap();
        let node1 = RaftNode::new(leader, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Send heartbeat - node 2's next_index should be 1, which is <= snapshot_last_index (3)
        let (_, _, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Node 2 should have received snapshot
        let node2_state = shared2.lock().await;
        assert_eq!(node2_state.snapshot_last_index, 4);
        assert_eq!(node2_state.snapshot_last_term, 1);
        assert_eq!(node2_state.last_applied, 4);
        // State machine should be restored (verified by successful handle_install_snapshot)
    }

    #[tokio::test]
    async fn test_heartbeat_handles_snapshot_boundary() {
        use crate::state_machine::kv::KeyValueStore;

        let node_ids = vec![1, 2];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        // Create leader with KV store
        let mut leader = RaftCore::new(
            1,
            vec![2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        leader.start_election();
        leader.become_leader(); // Appends NOOP at index 1
        leader.append_log_entry("SET x 1".to_string()).unwrap(); // index 2
        leader.append_log_entry("SET y 2".to_string()).unwrap(); // index 3

        // Apply and snapshot at index 3
        leader.commit_index = 3;
        leader.apply_committed_entries();
        leader.take_snapshot().unwrap();

        // Add one more entry AFTER snapshot
        leader.append_log_entry("SET z 3".to_string()).unwrap(); // index 4

        assert_eq!(leader.snapshot_last_index, 3);
        assert_eq!(leader.log.len(), 1); // Only entry at index 4

        let core2 = RaftCore::new(
            2,
            vec![1],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        let transport1 = transports.remove(&1).unwrap();
        let node1 = RaftNode::new(leader, transport1);
        let shared2 = Arc::new(Mutex::new(core2));

        let mut handle2 = handles.remove(&2).unwrap();

        // Node 2 starts at next_index=1, which is <= snapshot_last_index (2)
        // So it should get snapshot first
        let (_, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
        );

        let node2_state = shared2.lock().await;
        assert_eq!(node2_state.snapshot_last_index, 3);

        // After getting snapshot, next heartbeat should send entry at index 4
        drop(node2_state);

        let (_, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
        );

        let node2_state = shared2.lock().await;
        assert_eq!(node2_state.log.len(), 1);
        assert_eq!(node2_state.log[0].index, 4);
        assert_eq!(node2_state.log[0].command, "SET z 3");
    }

    #[tokio::test]
    async fn test_follower_catches_up_after_snapshot() {
        use crate::state_machine::kv::KeyValueStore;

        let node_ids = vec![1, 2];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        // Create leader with many entries
        let mut leader = RaftCore::new(
            1,
            vec![2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        leader.start_election();
        leader.become_leader(); // Appends NOOP at index 1

        // Add 10 entries (indices 2-11)
        for i in 1..=10 {
            let cmd = format!("SET key{} value{}", i, i);
            leader.append_log_entry(cmd).unwrap();
        }

        // Apply first 9 entries (NOOP + key1-key8) and snapshot
        leader.commit_index = 9;
        leader.apply_committed_entries();
        leader.take_snapshot().unwrap();

        // Apply remaining entries (key9 and key10 at indices 10-11)
        leader.commit_index = 11;
        leader.apply_committed_entries();

        assert_eq!(leader.snapshot_last_index, 9);
        assert_eq!(leader.log.len(), 2); // Entries 10 and 11 remain

        let core2 = RaftCore::new(
            2,
            vec![1],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        let transport1 = transports.remove(&1).unwrap();
        let node1 = RaftNode::new(leader, transport1);
        let shared2 = Arc::new(Mutex::new(core2));

        let mut handle2 = handles.remove(&2).unwrap();

        // First heartbeat - should send snapshot
        let (_, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
        );

        {
            let node2_state = shared2.lock().await;
            assert_eq!(node2_state.snapshot_last_index, 9);
            assert_eq!(node2_state.last_applied, 9);
            // State machine has first 9 entries (NOOP + key1-key8)
        }

        // Second heartbeat - should send entries 10 and 11 (key9 and key10)
        let (_, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
        );

        {
            let node2_state = shared2.lock().await;
            assert_eq!(node2_state.log.len(), 2);
            assert_eq!(node2_state.log[0].index, 10);
            assert_eq!(node2_state.log[1].index, 11);
        }

        // Third heartbeat - should commit entries 10 and 11
        let (_, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
        );

        {
            let node2_state = shared2.lock().await;
            assert_eq!(node2_state.commit_index, 11);
            assert_eq!(node2_state.last_applied, 11);
            // All 11 entries should be applied (NOOP + key1-key10)
        }
    }

    #[tokio::test]
    async fn test_mixed_snapshot_and_log_replication() {
        use crate::state_machine::kv::KeyValueStore;

        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        // Create leader
        let mut leader = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        leader.start_election();
        leader.become_leader();

        // Add 5 entries
        for i in 1..=5 {
            leader.append_log_entry(format!("SET k{} v{}", i, i)).unwrap();
        }

        // Apply first 3 and snapshot
        leader.commit_index = 3;
        leader.apply_committed_entries();
        leader.take_snapshot().unwrap();

        // Apply remaining entries
        leader.commit_index = 5;
        leader.apply_committed_entries();

        // Load the snapshot from the leader before wrapping it
        let snapshot = leader.load_snapshot().unwrap().unwrap();

        // Create followers
        let mut core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        // Give node2 the snapshot so it's caught up through index 3
        // This simulates a node that received the snapshot previously
        {
            let args = InstallSnapshotArgs {
                term: 1,
                leader_id: 1,
                last_included_index: snapshot.metadata.last_included_index,
                last_included_term: snapshot.metadata.last_included_term,
                data: snapshot.data,
            };
            core2.handle_install_snapshot(&args);
        }

        let core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        let transport1 = transports.remove(&1).unwrap();
        let node1 = RaftNode::new(leader, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        // Manually set next_index for node 2 (it has the snapshot, needs entries from 4 onwards)
        {
            let mut leader = node1.core.lock().await;
            leader.next_index.insert(2, 4);
        }

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Send heartbeat - node 2 should get AppendEntries (for entries 4-6),
        // node 3 should get snapshot
        let (_, _, _) = tokio::join!(
            node1.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Node 2 should have received log entries (it already had the snapshot)
        {
            let node2_state = shared2.lock().await;
            assert_eq!(node2_state.snapshot_last_index, 3); // Has snapshot up to index 3
            // Node 2 should have received entries 4-6 (SET k3, k4, k5) via AppendEntries
            assert_eq!(node2_state.log.len(), 3);
            assert_eq!(node2_state.log[0].index, 4);
            assert_eq!(node2_state.log[2].index, 6);
        }

        // Node 3 should have received snapshot
        {
            let node3_state = shared3.lock().await;
            assert_eq!(node3_state.snapshot_last_index, 3);
            assert_eq!(node3_state.last_applied, 3);
            // State machine has first 3 entries (NOOP, SET k1, SET k2)
        }
    }

    /// Test that replicate_to_peers works correctly AFTER a snapshot has been taken.
    /// This catches the bug where prev_log_term was computed using direct Vec indexing
    /// instead of accounting for snapshot offset.
    #[tokio::test]
    async fn test_replicate_to_peers_after_snapshot() {
        use crate::state_machine::kv::KeyValueStore;

        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        // Create leader with KeyValueStore (needed for snapshot)
        let core1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        // Create followers
        let core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        let core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        let transport1 = transports.remove(&1).unwrap();
        let node1 = RaftNode::new(core1, transport1);
        let shared1 = node1.shared_core();

        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Node 1 becomes leader
        node1.start_election().await;
        let (_, _, _) = tokio::join!(
            node1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(node1.state().await, RaftState::Leader);

        // Add entries 2-10 (NOOP is entry 1)
        for i in 1..=9 {
            shared1.lock().await.append_log_entry(format!("SET k{} v{}", i, i));
        }

        // Replicate all entries
        let (_, _, _) = tokio::join!(
            node1.replicate_to_peers(10),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Verify entries committed
        assert_eq!(shared1.lock().await.commit_index, 10);

        // Now MANUALLY take snapshot on leader (simulating what happens in real cluster)
        {
            let mut core = shared1.lock().await;
            core.apply_committed_entries(); // This would normally trigger auto-snapshot
            core.take_snapshot().unwrap();

            // Verify snapshot state
            assert_eq!(core.snapshot_last_index, 10);
            assert_eq!(core.snapshot_last_term, 1);
            assert!(core.log.is_empty(), "Log should be empty after snapshot");
        }

        // Now append a NEW entry AFTER snapshot
        let new_entry_index = {
            let mut core = shared1.lock().await;
            let entry = core.append_log_entry("SET k10 v10".to_string()).unwrap();
            entry.index
        };
        assert_eq!(new_entry_index, 11);

        // THIS IS THE KEY TEST: replicate_to_peers after snapshot
        // The bug was: prev_log_index=10, but log.get(9) returns None after snapshot
        // causing prev_log_term=0 instead of using snapshot_last_term=1
        let (_, _, _) = tokio::join!(
            node1.replicate_to_peers(11),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Verify entry 11 was committed (this will fail if prev_log_term was wrong)
        {
            let core1 = shared1.lock().await;
            assert_eq!(core1.commit_index, 11,
                "Entry 11 should be committed - if this fails, prev_log_term was wrong");
        }

        // Verify followers received entry 11
        {
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;
            assert_eq!(core2.last_log_index(), 11, "Follower 2 should have entry 11");
            assert_eq!(core3.last_log_index(), 11, "Follower 3 should have entry 11");
        }
    }
}
