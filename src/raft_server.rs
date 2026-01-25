//! RaftServer - High-level Raft server for handling client commands

use std::pin::pin;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, sleep_until, Instant};

use crate::config::RaftConfig;
use crate::raft_node::{RaftNode, SharedCore};
use crate::raft_core::{RaftCore, RaftState};
use crate::transport::{Transport, TransportError};

/// Errors that can occur during Raft operations
#[derive(Debug, Clone)]
pub enum RaftError {
    /// This node is not the leader (includes leader hint if known)
    NotLeader { leader_hint: Option<u64> },
    /// Entry was not committed (couldn't reach majority)
    NotCommitted,
    /// State machine returned an error
    StateMachine(String),
    /// Transport error occurred
    Transport(TransportError),
}

/// Command sent to the RaftServer from clients
enum Command {
    /// Submit a client command to be replicated
    Submit {
        command: String,
        reply: oneshot::Sender<Result<String, RaftError>>,
    },
}

/// Events that can be sent to the server loop
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// Reset the election timeout (received valid heartbeat from leader)
    ResetElectionTimeout,
}

/// Sender for server events (used by transport layer to notify server)
pub type EventSender = mpsc::Sender<ServerEvent>;

/// Handle for interacting with a running RaftServer
#[derive(Clone)]
pub struct RaftHandle {
    command_tx: mpsc::Sender<Command>,
}

impl RaftHandle {
    /// Submit a command to the Raft cluster
    /// Returns the state machine result if successful
    pub async fn submit(&self, command: String) -> Result<String, RaftError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(Command::Submit {
                command,
                reply: reply_tx,
            })
            .await
            .map_err(|_| RaftError::NotLeader { leader_hint: None })?;

        reply_rx.await.map_err(|_| RaftError::NotLeader { leader_hint: None })?
    }
}

/// Raft server that handles client commands and coordinates with RaftNode
pub struct RaftServer<T: Transport> {
    node: RaftNode<T>,
    command_rx: mpsc::Receiver<Command>,
    command_tx: mpsc::Sender<Command>,
    event_rx: mpsc::Receiver<ServerEvent>,
    config: RaftConfig,
}

impl<T: Transport + 'static> RaftServer<T> {
    /// Create a new RaftServer with default config
    /// Returns the server, shared core for RPC handling, and event sender for timeout resets
    pub fn new(core: RaftCore, transport: T) -> (Self, SharedCore, EventSender) {
        Self::with_config(core, transport, RaftConfig::default())
    }

    /// Create a new RaftServer with custom config
    /// Returns the server, shared core for RPC handling, and event sender for timeout resets
    pub fn with_config(core: RaftCore, transport: T, config: RaftConfig) -> (Self, SharedCore, EventSender) {
        let (command_tx, command_rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(32);
        let node = RaftNode::new(core, transport);
        let shared_core = node.shared_core();
        let server = Self {
            node,
            command_rx,
            command_tx,
            event_rx,
            config,
        };
        (server, shared_core, event_tx)
    }

    /// Start the server and return a handle for interaction
    pub fn start(self) -> RaftHandle {
        let handle = RaftHandle {
            command_tx: self.command_tx.clone(),
        };

        tokio::spawn(self.run());

        handle
    }

    /// Main server loop
    async fn run(mut self) {
        let mut heartbeat_interval = interval(self.config.heartbeat_interval);
        let mut election_deadline = Instant::now() + self.config.random_election_timeout();

        loop {
            // Create a fresh sleep future each iteration that will fire at election_deadline
            let election_sleep = pin!(sleep_until(election_deadline));

            tokio::select! {
                // Handle client commands
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        Command::Submit { command, reply } => {
                            let result = self.handle_submit(command).await;
                            let _ = reply.send(result);
                        }
                    }
                }
                // Handle events from transport layer (e.g., heartbeat received)
                Some(event) = self.event_rx.recv() => {
                    match event {
                        ServerEvent::ResetElectionTimeout => {
                            // Update deadline - next iteration will create new sleep with this deadline
                            election_deadline = Instant::now() + self.config.random_election_timeout();
                        }
                    }
                }
                // Send heartbeats if leader
                _ = heartbeat_interval.tick() => {
                    if self.node.state().await == RaftState::Leader {
                        self.node.send_heartbeat().await;
                    }
                }
                // Election timeout - start election if not leader
                _ = election_sleep => {
                    let state = self.node.state().await;
                    if state != RaftState::Leader {
                        // Start election
                        self.node.start_election().await;
                        let became_leader = self.node.request_votes().await;

                        if became_leader {
                            // Immediately send heartbeat to establish leadership
                            self.node.send_heartbeat().await;
                        }
                    }
                    // Reset election timeout for next iteration
                    election_deadline = Instant::now() + self.config.random_election_timeout();
                }
                else => break, // All channels closed, shutdown
            }
        }
    }

    /// Handle a client submit command
    /// Returns the state machine result if committed, or an error
    async fn handle_submit(&self, command: String) -> Result<String, RaftError> {
        let shared_core = self.node.shared_core();
        let entry_index = {
            let mut core = shared_core.lock().await;

            // Only leaders can accept commands
            if core.state != RaftState::Leader {
                return Err(RaftError::NotLeader { leader_hint: core.current_leader });
            }

            // Append to local log
            let entry = core.append_log_entry(command).ok_or(RaftError::NotLeader { leader_hint: None })?;
            entry.index
        };

        // Replicate to all peers and get state machine result
        match self.node.replicate_to_peers(entry_index).await {
            Some(Ok(result)) => Ok(result),
            Some(Err(err)) => Err(RaftError::StateMachine(err)),
            None => Err(RaftError::NotCommitted),
        }
    }

    /// Start an election (delegates to RaftNode)
    pub async fn start_election(&self) {
        self.node.start_election().await;
    }

    /// Request votes from all peers (delegates to RaftNode)
    pub async fn request_votes(&self) -> bool {
        self.node.request_votes().await
    }

    /// Get current state
    pub async fn state(&self) -> RaftState {
        self.node.state().await
    }

    /// Get commit index
    pub async fn commit_index(&self) -> u64 {
        self.node.commit_index().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use crate::config::RaftConfig;
    use crate::state_machine::{AppliedCommands, TestStateMachine};
    use crate::storage_memory::MemoryStorage;
    use crate::transport_inmemory::create_cluster;

    /// Helper to create RaftCore with MemoryStorage for tests
    fn new_test_core(id: u64, peers: Vec<u64>) -> RaftCore {
        RaftCore::new(
            id,
            peers,
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        )
    }

    /// Helper to create RaftCore with a shared state machine for verification
    fn new_test_core_with_shared(id: u64, peers: Vec<u64>, applied: AppliedCommands) -> RaftCore {
        RaftCore::new(
            id,
            peers,
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new_shared(applied)),
        )
    }

    #[tokio::test]
    async fn test_server_not_leader() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, _handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();

        let (server, _shared_core, _event_tx) = RaftServer::new(core1, transport1);
        let handle = server.start();

        // Node is not leader, should fail
        let result = handle.submit("SET x=1".to_string()).await;
        assert!(matches!(result, Err(RaftError::NotLeader { .. })));
    }

    #[tokio::test]
    async fn test_election_via_server() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, _shared1, _event_tx) = RaftServer::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        // Start election
        server1.start_election().await;

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Process vote requests concurrently
        let (became_leader, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        assert!(became_leader);
        assert_eq!(server1.state().await, RaftState::Leader);
    }

    #[tokio::test]
    async fn test_replication_via_server() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1, _event_tx) = RaftServer::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election first
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Submit a command
        let entry_index = {
            let mut core = shared1.lock().await;
            let entry = core.append_log_entry("SET x=1".to_string()).unwrap();
            entry.index
        };

        // Replicate to peers using the node directly
        let (_, _, _) = tokio::join!(
            server1.node.replicate_to_peers(entry_index),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Entry should be committed
        assert_eq!(server1.commit_index().await, entry_index);
        assert_eq!(shared2.lock().await.log.len(), 1);
        assert_eq!(shared3.lock().await.log.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_election_timeout_triggers_election() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        // With paused time, actual duration values don't affect test speed
        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_millis(300), Duration::from_millis(500));

        let (server1, shared1, _event_tx) = RaftServer::with_config(core1, transport1, config);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        // Verify node starts as follower
        assert_eq!(shared1.lock().await.state, RaftState::Follower);
        assert_eq!(shared1.lock().await.current_term, 0);

        // Start server (runs in background)
        let _handle = server1.start();

        // Get handles for processing incoming requests
        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Spawn task to process vote requests when they arrive
        let shared2_clone = shared2.clone();
        let shared3_clone = shared3.clone();
        tokio::spawn(async move {
            tokio::join!(
                handle2.process_one_shared(&shared2_clone),
                handle3.process_one_shared(&shared3_clone),
            );
        });

        // Advance time past election timeout (max is 500ms)
        // With paused time, we need to advance and yield multiple times
        // to let all tasks make progress
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
        }

        // Node 1 should have started election and become leader
        let state = shared1.lock().await.state;
        let term = shared1.lock().await.current_term;

        assert_eq!(state, RaftState::Leader, "Node should become leader after election timeout");
        assert!(term >= 1, "Term should have increased from election");
    }

    #[tokio::test(start_paused = true)]
    async fn test_no_election_before_timeout() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, _handles) = create_cluster(&node_ids);

        let core1 = new_test_core(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();

        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_millis(300), Duration::from_millis(500));

        let (server1, shared1, _event_tx) = RaftServer::with_config(core1, transport1, config);

        // Verify node starts as follower at term 0
        assert_eq!(shared1.lock().await.state, RaftState::Follower);
        assert_eq!(shared1.lock().await.current_term, 0);

        // Start server (runs in background)
        let _handle = server1.start();

        // Advance time but NOT past minimum election timeout (300ms)
        tokio::time::advance(Duration::from_millis(200)).await;

        // Yield to let server loop run
        tokio::task::yield_now().await;

        // Node should still be follower at term 0 (no election started)
        let state = shared1.lock().await.state;
        let term = shared1.lock().await.current_term;

        assert_eq!(state, RaftState::Follower, "Node should remain follower before election timeout");
        assert_eq!(term, 0, "Term should not have changed before election timeout");
    }

    // === Full Client Command Flow Tests ===

    #[tokio::test(start_paused = true)]
    async fn test_client_command_flow() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // Test: submit → replicate → commit → applied
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        // Use long election timeout so server doesn't start elections during test
        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100));

        let (server1, shared1, _event_tx) = RaftServer::with_config(core1, transport1, config);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election manually first (before starting server loop)
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Start the server loop (runs in background)
        let client_handle = server1.start();

        // Spawn tasks: submit and peer handlers
        let submit_task = tokio::spawn(async move {
            client_handle.submit("SET x=42".to_string()).await
        });

        let shared2_clone = shared2.clone();
        let shared3_clone = shared3.clone();
        tokio::spawn(async move {
            tokio::join!(
                handle2.process_one_shared(&shared2_clone),
                handle3.process_one_shared(&shared3_clone),
            );
        });

        // With start_paused, advance time to let all spawned tasks make progress
        // This is the same pattern used in test_election_timeout_triggers_election
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        // Get submit result
        let result = submit_task.await.unwrap();

        // Command should succeed (TestStateMachine returns empty string)
        assert!(result.is_ok());

        // Verify state on leader
        assert_eq!(shared1.lock().await.log.len(), 1);
        assert_eq!(shared1.lock().await.log[0].command, "SET x=42");
        assert_eq!(shared1.lock().await.commit_index, 1);
        assert_eq!(shared1.lock().await.last_applied, 1);

        // Verify state on followers
        assert_eq!(shared2.lock().await.log.len(), 1);
        assert_eq!(shared3.lock().await.log.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_multiple_commands_in_sequence() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100));

        let (server1, shared1, _event_tx) = RaftServer::with_config(core1, transport1, config);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election manually
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Start server loop
        let client_handle = server1.start();

        // Spawn tasks: submit commands and peer handlers
        let ch = client_handle.clone();
        let submit_task = tokio::spawn(async move {
            let r1 = ch.submit("SET x=1".to_string()).await;
            let r2 = ch.submit("SET y=2".to_string()).await;
            let r3 = ch.submit("SET z=3".to_string()).await;
            (r1, r2, r3)
        });

        let shared2_clone = shared2.clone();
        let shared3_clone = shared3.clone();
        tokio::spawn(async move {
            // Process 3 rounds of peer requests (one per command)
            for _ in 0..3 {
                tokio::join!(
                    handle2.process_one_shared(&shared2_clone),
                    handle3.process_one_shared(&shared3_clone),
                );
            }
        });

        // With start_paused, advance time to let all spawned tasks make progress
        for _ in 0..30 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        // Get submit results (all should succeed)
        let (result1, result2, result3) = submit_task.await.unwrap();
        assert!(result1.is_ok());
        assert!(result2.is_ok());
        assert!(result3.is_ok());

        // All entries should be committed
        assert_eq!(shared1.lock().await.commit_index, 3);
        assert_eq!(shared2.lock().await.log.len(), 3);
        assert_eq!(shared3.lock().await.log.len(), 3);
    }

    // === Leader Failover Tests ===

    #[tokio::test(start_paused = true)]
    async fn test_leader_failover() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // Scenario: Leader 1 commits entries, then "fails", Node 2 becomes new leader
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1, _) = RaftServer::new(core1, transport1);
        let (server2, shared2, _) = RaftServer::new(core2, transport2);
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Node 1 wins election
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);
        let term1 = shared1.lock().await.current_term;

        // Leader 1 commits an entry
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("CMD from leader 1".to_string());
        }
        let (_, _, _) = tokio::join!(
            server1.node.replicate_to_peers(1),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(shared1.lock().await.commit_index, 1);

        // Now node 1 "fails" - node 2 starts election with higher term
        server2.start_election().await;

        // Only node 3 responds (node 1 "failed" - times out)
        let (_, _) = tokio::join!(
            server2.request_votes(),
            handle3.process_one_shared(&shared3),
        );

        // Node 2 should become leader (has vote from self + node 3 = majority)
        assert_eq!(server2.state().await, RaftState::Leader);
        let term2 = shared2.lock().await.current_term;
        assert!(term2 > term1, "New leader should have higher term");

        // Verify node 2 still has the committed entry from leader 1
        assert_eq!(shared2.lock().await.log.len(), 1);
        assert_eq!(shared2.lock().await.log[0].command, "CMD from leader 1");
    }

    // === Network Partition Tests ===

    #[tokio::test(start_paused = true)]
    async fn test_leader_isolated_cannot_commit() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // Leader gets partitioned from majority - cannot commit new entries
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1, _) = RaftServer::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Node 1 wins election
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Now partition leader from both followers
        // Leader tries to append an entry but neither follower responds
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("CMD during partition".to_string());
        }

        // Replicate without processing - both will timeout
        server1.node.replicate_to_peers(1).await;

        // Entry should NOT be committed (no majority)
        assert_eq!(shared1.lock().await.log.len(), 1);
        assert_eq!(shared1.lock().await.commit_index, 0, "Should not commit without majority");
    }

    #[tokio::test(start_paused = true)]
    async fn test_split_brain_prevention() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // Two nodes think they're candidates, only one can become leader
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1, _) = RaftServer::new(core1, transport1);
        let (server2, shared2, _) = RaftServer::new(core2, transport2);
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle3 = handles.remove(&3).unwrap();

        // Both start elections at same term
        server1.start_election().await;
        server2.start_election().await;

        // Node 3 receives vote request from node 1 first, grants vote
        // Then receives vote request from node 2, denies (already voted)

        // Process node 1's vote request to node 3
        let (_, _) = tokio::join!(
            server1.request_votes(),
            handle3.process_one_shared(&shared3),
        );

        // Node 1 should be leader (self + node3 = majority)
        assert_eq!(server1.state().await, RaftState::Leader);

        // Node 2's election times out (node 1 and 3 don't respond to it)
        server2.request_votes().await;

        // Node 2 should still be candidate (only has self vote)
        assert_eq!(server2.state().await, RaftState::Candidate);

        // Verify terms
        let term1 = shared1.lock().await.current_term;
        let term2 = shared2.lock().await.current_term;
        assert_eq!(term1, term2); // Same term, but only one leader
    }

    #[tokio::test(start_paused = true)]
    async fn test_partitioned_node_rejoins() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // Node 3 is partitioned, misses some entries, then rejoins and catches up
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1, _) = RaftServer::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Node 1 wins election (all nodes participate)
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Commit entries while node 3 is "partitioned" (only node 2 responds)
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("CMD 1".to_string());
            core.append_log_entry("CMD 2".to_string());
        }

        // Replicate only to node 2 (node 3 is partitioned - times out)
        let (_, _) = tokio::join!(
            server1.node.replicate_to_peers(2),
            handle2.process_one_shared(&shared2),
        );

        // Entries committed on leader and node 2
        assert_eq!(shared1.lock().await.commit_index, 2);
        assert_eq!(shared2.lock().await.log.len(), 2);
        assert_eq!(shared3.lock().await.log.len(), 0); // Node 3 missed everything

        // Node 3 rejoins - leader sends heartbeat with catch-up entries
        let (_, _) = tokio::join!(
            server1.node.send_heartbeat(),
            handle3.process_one_shared(&shared3),
        );

        // Node 3 should now have all entries
        assert_eq!(shared3.lock().await.log.len(), 2);
        assert_eq!(shared3.lock().await.log[0].command, "CMD 1");
        assert_eq!(shared3.lock().await.log[1].command, "CMD 2");
    }

    #[tokio::test(start_paused = true)]
    async fn test_stale_leader_steps_down() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // Old leader (partitioned) tries to replicate, sees higher term, steps down
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1, _) = RaftServer::new(core1, transport1);
        let (server2, shared2, _) = RaftServer::new(core2, transport2);
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Node 1 becomes leader at term 1
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);
        assert_eq!(shared1.lock().await.current_term, 1);

        // Node 1 gets partitioned, node 2 becomes leader at term 2
        server2.start_election().await;
        let (_, _) = tokio::join!(
            server2.request_votes(),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server2.state().await, RaftState::Leader);
        assert_eq!(shared2.lock().await.current_term, 2);

        // Old leader (node 1) tries to send heartbeat to node 2
        // Node 2 will reject with higher term
        let (still_leader, _) = tokio::join!(
            server1.node.send_heartbeat(),
            handle2.process_one_shared(&shared2),
        );

        // Node 1 should step down after seeing higher term
        assert!(!still_leader);
        assert_eq!(shared1.lock().await.state, RaftState::Follower);
        assert_eq!(shared1.lock().await.current_term, 2); // Updated to higher term
    }

    #[tokio::test(start_paused = true)]
    async fn test_candidate_rejects_commands() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // A candidate should reject client commands (only leaders accept)
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, _handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();

        // Use long election timeout so we control when elections happen
        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100));

        let (server1, shared1, _) = RaftServer::with_config(core1, transport1, config);

        // Start election but don't let it complete (no peers respond)
        server1.start_election().await;

        // Verify node is now a candidate
        assert_eq!(shared1.lock().await.state, RaftState::Candidate);

        // Start server and try to submit command
        let client_handle = server1.start();

        let submit_task = tokio::spawn(async move {
            client_handle.submit("SET x=1".to_string()).await
        });

        // Advance time to let submit complete
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        let result = submit_task.await.unwrap();

        // Candidate should reject the command
        assert!(matches!(result, Err(RaftError::NotLeader { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn test_command_fails_when_leader_loses_leadership() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        // Leader starts replicating, but steps down mid-replication
        // when it sees a higher term from a peer
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let mut core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100));

        let (server1, shared1, _) = RaftServer::with_config(core1, transport1, config);
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Node 1 wins election at term 1
        server1.start_election().await;

        // Manually set node 2 to have a higher term (simulating it was in a different partition
        // and had an election). When node 1 sends AppendEntries, node 2 will reject with term 5.
        core2.current_term = 5;
        let shared2 = Arc::new(Mutex::new(core2));

        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Node 1 should have stepped down after seeing term 5 from node 2
        // (request_vote response with higher term causes step down)
        assert_eq!(shared1.lock().await.state, RaftState::Follower);
        assert_eq!(shared1.lock().await.current_term, 5);

        // Now start server and try to submit - should fail since not leader
        let client_handle = server1.start();

        let submit_task = tokio::spawn(async move {
            client_handle.submit("SET x=1".to_string()).await
        });

        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        let result = submit_task.await.unwrap();
        assert!(matches!(result, Err(RaftError::NotLeader { .. })));
    }

    // === State Machine Apply Integration Tests ===

    #[tokio::test(start_paused = true)]
    async fn test_entry_applied_when_quorum_reached() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create recording state machine for leader to verify applies
        let applied1: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let core1 = new_test_core_with_shared(1, vec![2, 3], applied1.clone());
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100));

        let (server1, shared1, _) = RaftServer::with_config(core1, transport1, config);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Verify nothing applied yet
        assert!(applied1.lock().unwrap().is_empty());

        // Start server and submit command
        let client_handle = server1.start();

        let submit_task = tokio::spawn(async move {
            client_handle.submit("SET x=42".to_string()).await
        });

        let shared2_clone = shared2.clone();
        let shared3_clone = shared3.clone();
        tokio::spawn(async move {
            tokio::join!(
                handle2.process_one_shared(&shared2_clone),
                handle3.process_one_shared(&shared3_clone),
            );
        });

        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        let result = submit_task.await.unwrap();
        assert!(result.is_ok());

        // Entry should be committed AND applied on leader
        assert_eq!(shared1.lock().await.commit_index, 1);
        assert_eq!(shared1.lock().await.last_applied, 1);

        // Verify the command was actually applied to state machine
        let applied = applied1.lock().unwrap();
        assert_eq!(applied.len(), 1, "One command should be applied");
        assert_eq!(applied[0], "SET x=42", "Correct command should be applied");
    }

    #[tokio::test(start_paused = true)]
    async fn test_entry_not_applied_without_quorum() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create recording state machine for leader
        let applied1: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let core1 = new_test_core_with_shared(1, vec![2, 3], applied1.clone());
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1, _) = RaftServer::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Append entry directly (simulating client command)
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("CMD during partition".to_string());
        }

        // Replicate but don't process peer requests (simulating partition)
        // Both peers will timeout
        server1.node.replicate_to_peers(1).await;

        // Entry should be in log but NOT committed, NOT applied
        assert_eq!(shared1.lock().await.log.len(), 1);
        assert_eq!(shared1.lock().await.commit_index, 0, "Should not commit without quorum");
        assert_eq!(shared1.lock().await.last_applied, 0, "Should not apply without commit");

        // Verify command was NOT applied to state machine
        let applied = applied1.lock().unwrap();
        assert!(applied.is_empty(), "No commands should be applied without quorum");
    }

    #[tokio::test(start_paused = true)]
    async fn test_follower_applies_entry_on_commit_notification() {
        use crate::transport_inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create shared state machines for all nodes
        let applied1: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let applied2: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let applied3: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));

        let core1 = new_test_core_with_shared(1, vec![2, 3], applied1.clone());
        let core2 = new_test_core_with_shared(2, vec![1, 3], applied2.clone());
        let core3 = new_test_core_with_shared(3, vec![1, 2], applied3.clone());

        let transport1 = transports.remove(&1).unwrap();

        // Short heartbeat interval so we can trigger it by advancing time
        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100))
            .with_heartbeat_interval(Duration::from_millis(50));

        let (server1, shared1, _) = RaftServer::with_config(core1, transport1, config);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Start server
        let client_handle = server1.start();

        // Spawn peer handlers that will process multiple requests (replication + heartbeat)
        let shared2_clone = shared2.clone();
        let shared3_clone = shared3.clone();
        tokio::spawn(async move {
            // Process replication request
            tokio::join!(
                handle2.process_one_shared(&shared2_clone),
                handle3.process_one_shared(&shared3_clone),
            );
            // Process heartbeat request
            tokio::join!(
                handle2.process_one_shared(&shared2_clone),
                handle3.process_one_shared(&shared3_clone),
            );
        });

        // Submit command
        let submit_task = tokio::spawn(async move {
            client_handle.submit("SET x=42".to_string()).await
        });

        // Advance time to let replication complete
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        submit_task.await.unwrap().unwrap();

        // Leader should have committed and applied
        assert_eq!(shared1.lock().await.commit_index, 1);
        assert_eq!(shared1.lock().await.last_applied, 1);

        // Verify leader's state machine received the command
        {
            let leader_applied = applied1.lock().unwrap();
            assert_eq!(leader_applied.len(), 1);
            assert_eq!(leader_applied[0], "SET x=42");
        }

        // Followers have entry but may not have applied yet
        assert_eq!(shared2.lock().await.log.len(), 1);
        assert_eq!(shared3.lock().await.log.len(), 1);

        // Advance time past heartbeat interval to trigger heartbeat with updated commit_index
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        // Now followers should have applied
        assert_eq!(shared2.lock().await.commit_index, 1);
        assert_eq!(shared2.lock().await.last_applied, 1);
        assert_eq!(shared3.lock().await.commit_index, 1);
        assert_eq!(shared3.lock().await.last_applied, 1);

        // Verify followers' state machines received the command
        {
            let follower2_applied = applied2.lock().unwrap();
            assert_eq!(follower2_applied.len(), 1, "Follower 2 should have applied");
            assert_eq!(follower2_applied[0], "SET x=42");
        }
        {
            let follower3_applied = applied3.lock().unwrap();
            assert_eq!(follower3_applied.len(), 1, "Follower 3 should have applied");
            assert_eq!(follower3_applied[0], "SET x=42");
        }
    }
}
