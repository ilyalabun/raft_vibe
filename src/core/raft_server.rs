//! RaftServer - High-level Raft server for handling client commands

use std::pin::pin;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, sleep_until, Duration, Instant, MissedTickBehavior};

use super::config::RaftConfig;
use super::raft_node::{RaftNode, SharedCore};
use super::raft_core::{RaftCore, RaftState};
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

/// Handle for interacting with a running RaftServer
#[derive(Clone)]
pub struct RaftHandle {
    command_tx: mpsc::Sender<Command>,
    shutdown_tx: mpsc::Sender<()>,
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

    /// Shutdown the RaftServer gracefully
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(()).await;
    }
}

/// Raft server that handles client commands and coordinates with RaftNode
pub struct RaftServer<T: Transport> {
    node: RaftNode<T>,
    command_rx: mpsc::Receiver<Command>,
    command_tx: mpsc::Sender<Command>,
    shutdown_rx: mpsc::Receiver<()>,
    shutdown_tx: mpsc::Sender<()>,
    config: RaftConfig,
}

impl<T: Transport + 'static> RaftServer<T> {
    /// Create a new RaftServer with default config
    /// Returns the server and shared core for RPC handling
    pub fn new(core: RaftCore, transport: T) -> (Self, SharedCore) {
        Self::with_config(core, transport, RaftConfig::default())
    }

    /// Create a new RaftServer with custom config
    /// Returns the server and shared core for RPC handling
    pub fn with_config(core: RaftCore, transport: T, config: RaftConfig) -> (Self, SharedCore) {
        let (command_tx, command_rx) = mpsc::channel(32);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let node = RaftNode::new(core, transport);
        let shared_core = node.shared_core();
        let server = Self {
            node,
            command_rx,
            command_tx,
            shutdown_rx,
            shutdown_tx,
            config,
        };
        (server, shared_core)
    }

    /// Start the server and return a handle for interaction
    pub fn start(self) -> RaftHandle {
        let handle = RaftHandle {
            command_tx: self.command_tx.clone(),
            shutdown_tx: self.shutdown_tx.clone(),
        };

        tokio::spawn(self.run());

        handle
    }

    /// Main server loop
    async fn run(mut self) {
        let mut heartbeat_interval = interval(self.config.heartbeat_interval);
        // Use Delay behavior to prevent accumulated missed ticks from starving election timeout
        heartbeat_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Use a fixed election timeout duration for this server instance
        let election_timeout = self.config.random_election_timeout();

        loop {
            // Calculate election deadline based on last_heartbeat from core
            let election_deadline = self.get_election_deadline(election_timeout).await;
            let election_sleep = pin!(sleep_until(election_deadline));

            tokio::select! {
                // Handle shutdown signal
                _ = self.shutdown_rx.recv() => {
                    break;
                }
                // Handle client commands
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        Command::Submit { command, reply } => {
                            let result = self.handle_submit(command).await;
                            let _ = reply.send(result);
                        }
                    }
                }
                // Send heartbeats if leader
                _ = heartbeat_interval.tick() => {
                    if self.node.state().await == RaftState::Leader {
                        // Update our own heartbeat timer to prevent election timeout
                        self.node.shared_core().lock().await.last_heartbeat = Instant::now();
                        self.node.send_heartbeat().await;
                    }
                }
                // Election timeout - start election if not leader
                _ = election_sleep => {
                    let state = self.node.state().await;
                    if state != RaftState::Leader {
                        // Check if we actually timed out (last_heartbeat might have been updated)
                        if self.has_election_timed_out(election_timeout).await {
                            // Start election
                            self.node.start_election().await;
                            let became_leader = self.node.request_votes().await;

                            if became_leader {
                                // Immediately send heartbeat to establish leadership
                                self.node.send_heartbeat().await;
                            }
                        }
                    }
                }
                else => break, // All channels closed, shutdown
            }
        }
    }

    /// Get election deadline based on last_heartbeat from core
    async fn get_election_deadline(&self, timeout: Duration) -> Instant {
        let core = self.node.shared_core();
        let last_heartbeat = core.lock().await.last_heartbeat;
        last_heartbeat + timeout
    }

    /// Check if election has actually timed out (last_heartbeat + timeout < now)
    async fn has_election_timed_out(&self, timeout: Duration) -> bool {
        let core = self.node.shared_core();
        let last_heartbeat = core.lock().await.last_heartbeat;
        Instant::now() >= last_heartbeat + timeout
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
    use crate::core::config::RaftConfig;
    use crate::state_machine::{AppliedCommands, TestStateMachine};
    use crate::storage::memory::MemoryStorage;
    use crate::transport::inmemory::create_cluster;

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

        let (server, _shared_core, ) = RaftServer::new(core1, transport1);
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

        let (server1, _shared1, ) = RaftServer::new(core1, transport1);
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

        let (server1, shared1, ) = RaftServer::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Win election first (become_leader appends NOOP)
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Submit a command (NOOP is at index 1, command at index 2)
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

        // Entry should be committed (NOOP + command)
        assert_eq!(server1.commit_index().await, entry_index);
        assert_eq!(shared2.lock().await.log.len(), 2);
        assert_eq!(shared3.lock().await.log.len(), 2);
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

        let (server1, shared1, ) = RaftServer::with_config(core1, transport1, config);
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

        let (server1, shared1, ) = RaftServer::with_config(core1, transport1, config);

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
        use crate::transport::inmemory::create_cluster_with_timeout;

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

        let (server1, shared1, ) = RaftServer::with_config(core1, transport1, config);
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

        // Verify state on leader (NOOP at index 1 + command at index 2)
        assert_eq!(shared1.lock().await.log.len(), 2);
        assert_eq!(shared1.lock().await.log[1].command, "SET x=42");
        assert_eq!(shared1.lock().await.commit_index, 2);
        assert_eq!(shared1.lock().await.last_applied, 2);

        // Verify state on followers
        assert_eq!(shared2.lock().await.log.len(), 2);
        assert_eq!(shared3.lock().await.log.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_multiple_commands_in_sequence() {
        use crate::transport::inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100));

        let (server1, shared1, ) = RaftServer::with_config(core1, transport1, config);
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

        // All entries should be committed (NOOP + 3 commands)
        assert_eq!(shared1.lock().await.commit_index, 4);
        assert_eq!(shared2.lock().await.log.len(), 4);
        assert_eq!(shared3.lock().await.log.len(), 4);
    }

    // === Leader Failover Tests ===

    #[tokio::test(start_paused = true)]
    async fn test_leader_failover() {
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Scenario: Leader 1 commits entries, then "fails", Node 2 becomes new leader
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
        let (server2, shared2) = RaftServer::new(core2, transport2);
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

        // Leader 1 commits an entry (NOOP is at index 1, command at index 2)
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("CMD from leader 1".to_string());
        }
        let (_, _, _) = tokio::join!(
            server1.node.replicate_to_peers(2), // Command is at index 2
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(shared1.lock().await.commit_index, 2);

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

        // Verify node 2 still has the committed entries from leader 1 + its own NOOP
        // Log: [NOOP from leader1, CMD from leader1, NOOP from leader2]
        assert_eq!(shared2.lock().await.log.len(), 3);
        assert_eq!(shared2.lock().await.log[1].command, "CMD from leader 1");
    }

    // === Network Partition Tests ===

    #[tokio::test(start_paused = true)]
    async fn test_leader_isolated_cannot_commit() {
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Leader gets partitioned from majority - cannot commit new entries
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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
        // (NOOP is at index 1, command at index 2)
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("CMD during partition".to_string());
        }

        // Replicate without processing - both will timeout
        server1.node.replicate_to_peers(2).await;

        // Entry should NOT be committed (no majority)
        // Leader has NOOP + command = 2 entries, but commit_index still 0
        assert_eq!(shared1.lock().await.log.len(), 2);
        assert_eq!(shared1.lock().await.commit_index, 0, "Should not commit without majority");
    }

    #[tokio::test(start_paused = true)]
    async fn test_split_brain_prevention() {
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Two nodes think they're candidates, only one can become leader
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
        let (server2, shared2) = RaftServer::new(core2, transport2);
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
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Node 3 is partitioned, misses some entries, then rejoins and catches up
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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
        // NOOP is at index 1, CMD 1 at index 2, CMD 2 at index 3
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("CMD 1".to_string());
            core.append_log_entry("CMD 2".to_string());
        }

        // Replicate only to node 2 (node 3 is partitioned - times out)
        let (_, _) = tokio::join!(
            server1.node.replicate_to_peers(3), // CMD 2 is at index 3
            handle2.process_one_shared(&shared2),
        );

        // Entries committed on leader and node 2 (NOOP + 2 commands)
        assert_eq!(shared1.lock().await.commit_index, 3);
        assert_eq!(shared2.lock().await.log.len(), 3);
        assert_eq!(shared3.lock().await.log.len(), 0); // Node 3 missed everything

        // Node 3 rejoins - leader sends heartbeat with catch-up entries
        let (_, _) = tokio::join!(
            server1.node.send_heartbeat(),
            handle3.process_one_shared(&shared3),
        );

        // Node 3 should now have all entries (NOOP + 2 commands)
        assert_eq!(shared3.lock().await.log.len(), 3);
        assert_eq!(shared3.lock().await.log[1].command, "CMD 1");
        assert_eq!(shared3.lock().await.log[2].command, "CMD 2");
    }

    #[tokio::test(start_paused = true)]
    async fn test_stale_leader_steps_down() {
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Old leader (partitioned) tries to replicate, sees higher term, steps down
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
        let (server2, shared2) = RaftServer::new(core2, transport2);
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
        use crate::transport::inmemory::create_cluster_with_timeout;

        // A candidate should reject client commands (only leaders accept)
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, _handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();

        // Use long election timeout so we control when elections happen
        let config = RaftConfig::default()
            .with_election_timeout(Duration::from_secs(100), Duration::from_secs(100));

        let (server1, shared1) = RaftServer::with_config(core1, transport1, config);

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
        use crate::transport::inmemory::create_cluster_with_timeout;

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

        let (server1, shared1) = RaftServer::with_config(core1, transport1, config);
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
        use crate::transport::inmemory::create_cluster_with_timeout;

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

        let (server1, shared1) = RaftServer::with_config(core1, transport1, config);
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

        // Entry should be committed AND applied on leader (NOOP + command)
        assert_eq!(shared1.lock().await.commit_index, 2);
        assert_eq!(shared1.lock().await.last_applied, 2);

        // Verify the commands were actually applied to state machine (NOOP + SET)
        let applied = applied1.lock().unwrap();
        assert_eq!(applied.len(), 2, "NOOP and command should be applied");
        assert_eq!(applied[1], "SET x=42", "Correct command should be applied");
    }

    #[tokio::test(start_paused = true)]
    async fn test_entry_not_applied_without_quorum() {
        use crate::transport::inmemory::create_cluster_with_timeout;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create recording state machine for leader
        let applied1: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let core1 = new_test_core_with_shared(1, vec![2, 3], applied1.clone());
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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
        // Both peers will timeout (command is at index 2 after NOOP)
        server1.node.replicate_to_peers(2).await;

        // Entry should be in log but NOT committed, NOT applied (NOOP + command)
        assert_eq!(shared1.lock().await.log.len(), 2);
        assert_eq!(shared1.lock().await.commit_index, 0, "Should not commit without quorum");
        assert_eq!(shared1.lock().await.last_applied, 0, "Should not apply without commit");

        // Verify command was NOT applied to state machine
        let applied = applied1.lock().unwrap();
        assert!(applied.is_empty(), "No commands should be applied without quorum");
    }

    #[tokio::test(start_paused = true)]
    async fn test_follower_applies_entry_on_commit_notification() {
        use crate::transport::inmemory::create_cluster_with_timeout;

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

        let (server1, shared1) = RaftServer::with_config(core1, transport1, config);
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

        // Leader should have committed and applied (NOOP + command)
        assert_eq!(shared1.lock().await.commit_index, 2);
        assert_eq!(shared1.lock().await.last_applied, 2);

        // Verify leader's state machine received the commands (NOOP + SET)
        {
            let leader_applied = applied1.lock().unwrap();
            assert_eq!(leader_applied.len(), 2);
            assert_eq!(leader_applied[1], "SET x=42");
        }

        // Followers have entries but may not have applied yet (NOOP + command)
        assert_eq!(shared2.lock().await.log.len(), 2);
        assert_eq!(shared3.lock().await.log.len(), 2);

        // Advance time past heartbeat interval to trigger heartbeat with updated commit_index
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        // Now followers should have applied
        assert_eq!(shared2.lock().await.commit_index, 2);
        assert_eq!(shared2.lock().await.last_applied, 2);
        assert_eq!(shared3.lock().await.commit_index, 2);
        assert_eq!(shared3.lock().await.last_applied, 2);

        // Verify followers' state machines received the commands (NOOP + SET)
        {
            let follower2_applied = applied2.lock().unwrap();
            assert_eq!(follower2_applied.len(), 2, "Follower 2 should have applied NOOP + command");
            assert_eq!(follower2_applied[1], "SET x=42");
        }
        {
            let follower3_applied = applied3.lock().unwrap();
            assert_eq!(follower3_applied.len(), 2, "Follower 3 should have applied NOOP + command");
            assert_eq!(follower3_applied[1], "SET x=42");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_state_machine_recovery_after_reelection() {
        use crate::core::raft_core::NOOP_COMMAND;
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Scenario: Leader 1 commits entries, then fails.
        // Leader 2 wins election and appends NOOP.
        // When NOOP is committed, prior entries should be applied on new leader.

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create shared state machines to verify applies
        let applied1: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let applied2: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let applied3: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));

        let core1 = new_test_core_with_shared(1, vec![2, 3], applied1.clone());
        let core2 = new_test_core_with_shared(2, vec![1, 3], applied2.clone());
        let core3 = new_test_core_with_shared(3, vec![1, 2], applied3.clone());

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
        let (server2, shared2) = RaftServer::new(core2, transport2);
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // === Phase 1: Leader 1 wins election and commits entries ===
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Leader 1 appends and commits entries
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("SET x=1".to_string());
            core.append_log_entry("SET y=2".to_string());
        }

        // Replicate to majority (entries at indices 2 and 3, after NOOP at 1)
        let (_, _, _) = tokio::join!(
            server1.node.replicate_to_peers(3),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Verify leader 1 committed and applied
        assert_eq!(shared1.lock().await.commit_index, 3);
        {
            let applied = applied1.lock().unwrap();
            assert_eq!(applied.len(), 3); // NOOP + 2 commands
            assert_eq!(applied[1], "SET x=1");
            assert_eq!(applied[2], "SET y=2");
        }

        // Followers have entries replicated but not yet committed
        assert_eq!(shared2.lock().await.log.len(), 3);
        assert_eq!(shared3.lock().await.log.len(), 3);

        // === Phase 2: Leader 1 "fails", Leader 2 wins election ===
        // Leader 2 starts election with higher term
        server2.start_election().await;

        // Only node 3 responds (node 1 "failed")
        let (_, _) = tokio::join!(
            server2.request_votes(),
            handle3.process_one_shared(&shared3),
        );

        // Leader 2 should win (self + node 3 = majority)
        assert_eq!(server2.state().await, RaftState::Leader);
        assert!(shared2.lock().await.current_term > 1);

        // Leader 2 has appended its own NOOP at index 4
        assert_eq!(shared2.lock().await.log.len(), 4);
        assert_eq!(shared2.lock().await.log[3].command, NOOP_COMMAND);

        // === Phase 3: Leader 2 replicates NOOP, commits all prior entries ===
        // Replicate to node 3 (node 1 still "failed")
        let (_, _) = tokio::join!(
            server2.node.replicate_to_peers(4),
            handle3.process_one_shared(&shared3),
        );

        // Leader 2 should now have committed all entries including its NOOP
        assert_eq!(shared2.lock().await.commit_index, 4);

        // Verify Leader 2's state machine now has all applied entries
        {
            let applied = applied2.lock().unwrap();
            assert_eq!(applied.len(), 4); // NOOP1 + SET x=1 + SET y=2 + NOOP2
            assert_eq!(applied[0], NOOP_COMMAND);
            assert_eq!(applied[1], "SET x=1");
            assert_eq!(applied[2], "SET y=2");
            assert_eq!(applied[3], NOOP_COMMAND);
        }

        // Node 3 should also have all entries applied after receiving commit notification
        assert_eq!(shared3.lock().await.log.len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn test_uncommitted_entries_committed_via_noop() {
        use crate::core::raft_core::NOOP_COMMAND;
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Scenario: 5-node cluster. Leader 1 replicates entries to only ONE follower.
        // 2/5 is minority, so entries are NOT committed. Leader 1 crashes.
        // Leader 2 (which has those entries) wins election.
        // Leader 2's NOOP commits all prior entries.

        let node_ids = vec![1, 2, 3, 4, 5];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let applied2: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));

        let core1 = new_test_core(1, vec![2, 3, 4, 5]);
        let core2 = new_test_core_with_shared(2, vec![1, 3, 4, 5], applied2.clone());
        let core3 = new_test_core(3, vec![1, 2, 4, 5]);
        let core4 = new_test_core(4, vec![1, 2, 3, 5]);
        let core5 = new_test_core(5, vec![1, 2, 3, 4]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
        let (server2, shared2) = RaftServer::new(core2, transport2);
        let shared3 = Arc::new(Mutex::new(core3));
        let shared4 = Arc::new(Mutex::new(core4));
        let shared5 = Arc::new(Mutex::new(core5));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();
        let mut handle4 = handles.remove(&4).unwrap();
        let mut handle5 = handles.remove(&5).unwrap();

        // === Phase 1: Leader 1 wins election ===
        server1.start_election().await;
        let (_, _, _, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
            handle4.process_one_shared(&shared4),
            handle5.process_one_shared(&shared5),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Leader 1 appends entries
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("SET x=1".to_string());
            core.append_log_entry("SET y=2".to_string());
        }

        // Replicate to node 2 ONLY (others time out)
        // 2/5 is minority, so entries are NOT committed
        let (_, _) = tokio::join!(
            server1.node.replicate_to_peers(3),
            handle2.process_one_shared(&shared2),
            // others not processed - timeout
        );

        // Verify: Leader 1 did NOT commit (only 1 peer responded, need 3 for majority of 5)
        assert_eq!(shared1.lock().await.commit_index, 0);
        // But node 2 has the entries replicated
        assert_eq!(shared2.lock().await.log.len(), 3); // NOOP + 2 entries
        // Other nodes have nothing
        assert_eq!(shared3.lock().await.log.len(), 0);
        assert_eq!(shared4.lock().await.log.len(), 0);
        assert_eq!(shared5.lock().await.log.len(), 0);

        // Node 2's state machine has NOT applied anything (not committed)
        assert!(applied2.lock().unwrap().is_empty());

        // === Phase 2: Leader 1 "crashes", Leader 2 wins election ===
        // Drain leftover AppendEntries that timed out (simulates lost requests)
        handle3.drain_pending();
        handle4.drain_pending();
        handle5.drain_pending();

        // Node 2 has the most up-to-date log, so it can become leader
        server2.start_election().await;

        // Nodes 3, 4, 5 vote for node 2 (node 1 "crashed")
        let (_, _, _, _) = tokio::join!(
            server2.request_votes(),
            handle3.process_one_shared(&shared3),
            handle4.process_one_shared(&shared4),
            handle5.process_one_shared(&shared5),
        );

        assert_eq!(server2.state().await, RaftState::Leader);

        // Leader 2 now has: [NOOP1, SET x=1, SET y=2, NOOP2]
        assert_eq!(shared2.lock().await.log.len(), 4);

        // === Phase 3: Leader 2 replicates NOOP, commits ALL entries ===
        // Drain any leftover requests from previous phases
        handle3.drain_pending();
        handle4.drain_pending();
        handle5.drain_pending();

        // Multiple heartbeats needed for catch-up:
        // - next_index starts at 4 (set before NOOP was appended)
        // - Followers have empty logs, so they reject until next_index = 1
        // - Each rejection decrements next_index
        for _ in 0..5 {
            let (_, _, _) = tokio::join!(
                server2.node.send_heartbeat(),
                handle3.process_one_shared(&shared3),
                handle4.process_one_shared(&shared4),
            );
            if shared2.lock().await.commit_index >= 4 {
                break;
            }
        }
        assert_eq!(shared2.lock().await.commit_index, 4);

        // Verify state machine on Leader 2 now has all entries applied
        {
            let applied = applied2.lock().unwrap();
            assert_eq!(applied.len(), 4);
            assert_eq!(applied[0], NOOP_COMMAND);
            assert_eq!(applied[1], "SET x=1");
            assert_eq!(applied[2], "SET y=2");
            assert_eq!(applied[3], NOOP_COMMAND);
        }

        // Nodes 3 and 4 should have caught up with all entries
        {
            let core3 = shared3.lock().await;
            assert_eq!(core3.log.len(), 4);
            assert_eq!(core3.log[0].command, NOOP_COMMAND);
            assert_eq!(core3.log[1].command, "SET x=1");
            assert_eq!(core3.log[2].command, "SET y=2");
            assert_eq!(core3.log[3].command, NOOP_COMMAND);
        }
        {
            let core4 = shared4.lock().await;
            assert_eq!(core4.log.len(), 4);
            assert_eq!(core4.log[0].command, NOOP_COMMAND);
            assert_eq!(core4.log[1].command, "SET x=1");
            assert_eq!(core4.log[2].command, "SET y=2");
            assert_eq!(core4.log[3].command, NOOP_COMMAND);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_fresh_node_catches_up_from_empty() {
        use crate::core::raft_core::NOOP_COMMAND;
        use crate::transport::inmemory::create_cluster_with_timeout;

        // Scenario: Node 3 starts with empty log while leader has committed entries.
        // When node 3 receives heartbeats, it catches up on all entries.

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let applied3: AppliedCommands = Arc::new(std::sync::Mutex::new(Vec::new()));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core_with_shared(3, vec![1, 2], applied3.clone());

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
        let shared2 = Arc::new(Mutex::new(core2));
        let shared3 = Arc::new(Mutex::new(core3));

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // === Phase 1: Leader commits entries with only node 2 ===
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Add entries and replicate only to node 2 (node 3 "partitioned")
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("SET a=1".to_string());
            core.append_log_entry("SET b=2".to_string());
        }

        // Replicate to node 2 only (node 3 times out)
        let (_, _) = tokio::join!(
            server1.node.replicate_to_peers(3),
            handle2.process_one_shared(&shared2),
        );

        // Verify leader committed (leader + node2 = majority)
        assert_eq!(shared1.lock().await.commit_index, 3);
        assert_eq!(shared2.lock().await.log.len(), 3);
        assert_eq!(shared3.lock().await.log.len(), 0); // Node 3 has nothing

        // === Phase 2: Node 3 "rejoins" and catches up via heartbeat ===
        handle3.drain_pending();

        // Send heartbeats until node 3 catches up (multiple rounds for next_index decrement)
        for _ in 0..5 {
            let (_, _) = tokio::join!(
                server1.node.send_heartbeat(),
                handle3.process_one_shared(&shared3),
            );
            if shared3.lock().await.log.len() >= 3 {
                break;
            }
        }

        // Node 3 should have all entries
        {
            let core3 = shared3.lock().await;
            assert_eq!(core3.log.len(), 3);
            assert_eq!(core3.log[0].command, NOOP_COMMAND);
            assert_eq!(core3.log[1].command, "SET a=1");
            assert_eq!(core3.log[2].command, "SET b=2");
        }

        // Node 3 should have applied entries (commit notification in heartbeat)
        {
            let applied = applied3.lock().unwrap();
            assert_eq!(applied.len(), 3);
            assert_eq!(applied[0], NOOP_COMMAND);
            assert_eq!(applied[1], "SET a=1");
            assert_eq!(applied[2], "SET b=2");
        }
    }

    /// High-level integration test that uses heartbeats for replication
    /// Tests that replication continues to work after automatic snapshot
    #[tokio::test(start_paused = true)]
    async fn test_snapshot_with_heartbeat_replication() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::storage::memory::MemoryStorage;
        use crate::transport::inmemory::create_cluster_with_timeout;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create cores with KeyValueStore and low snapshot threshold
        let mut core1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core1.set_snapshot_threshold(5);

        let mut core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core2.set_snapshot_threshold(5);

        let mut core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core3.set_snapshot_threshold(5);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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

        // === Phase 1: Add entries and use heartbeats to replicate ===
        // Add 10 entries (will trigger snapshot at 5)
        for i in 1..=10 {
            {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
            }
        }

        // Use heartbeats to replicate all entries
        // Multiple rounds needed for catch-up
        for round in 0..15 {
            let (_, _, _) = tokio::join!(
                server1.node.send_heartbeat(),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );

            // Check if all entries committed
            let commit_idx = shared1.lock().await.commit_index;
            if commit_idx >= 11 {
                println!("All entries committed after {} heartbeat rounds", round + 1);
                break;
            }
        }

        // Verify leader state after phase 1
        {
            let core = shared1.lock().await;
            println!("Leader: commit_index={}, snapshot_last_index={}, log.len={}",
                     core.commit_index, core.snapshot_last_index, core.log.len());
            assert!(core.snapshot_last_index > 0, "Leader should have taken snapshot");
            assert!(core.commit_index >= 11, "All 11 entries should be committed (NOOP + 10)");
        }

        // === Phase 2: Add more entries AFTER snapshot ===
        for i in 11..=15 {
            {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
            }
        }

        // Use heartbeats to replicate the new entries
        for round in 0..15 {
            let (_, _, _) = tokio::join!(
                server1.node.send_heartbeat(),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );

            // Check if all entries committed
            let commit_idx = shared1.lock().await.commit_index;
            let last_log = shared1.lock().await.last_log_index();
            if commit_idx >= last_log {
                println!("Phase 2: All entries committed after {} heartbeat rounds", round + 1);
                break;
            }
        }

        // Verify final state
        {
            let core1 = shared1.lock().await;
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;

            let leader_last = core1.last_log_index();
            println!("Final state:");
            println!("  Leader: last_log={}, commit={}, snapshot={}",
                     leader_last, core1.commit_index, core1.snapshot_last_index);
            println!("  Follower 2: last_log={}, commit={}, snapshot={}",
                     core2.last_log_index(), core2.commit_index, core2.snapshot_last_index);
            println!("  Follower 3: last_log={}, commit={}, snapshot={}",
                     core3.last_log_index(), core3.commit_index, core3.snapshot_last_index);

            // All nodes should have same last_log_index
            assert_eq!(core2.last_log_index(), leader_last,
                       "Follower 2 should match leader's last_log_index");
            assert_eq!(core3.last_log_index(), leader_last,
                       "Follower 3 should match leader's last_log_index");

            // All entries should be committed
            assert_eq!(core1.commit_index, leader_last,
                       "Leader should have committed all entries");
        }
    }

    /// Same as test_snapshot_with_heartbeat_replication but with FileStorage
    #[tokio::test(start_paused = true)]
    async fn test_snapshot_with_file_storage() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::storage::file::FileStorage;
        use crate::transport::inmemory::create_cluster_with_timeout;
        use std::sync::Arc;
        use tempfile::TempDir;
        use tokio::sync::Mutex;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create temp directories for each node
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        let dir3 = TempDir::new().unwrap();

        // Create cores with FileStorage and low snapshot threshold
        let mut core1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(FileStorage::new(dir1.path().to_str().unwrap()).unwrap()),
            Box::new(KeyValueStore::new()),
        );
        core1.set_snapshot_threshold(5);

        let mut core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(FileStorage::new(dir2.path().to_str().unwrap()).unwrap()),
            Box::new(KeyValueStore::new()),
        );
        core2.set_snapshot_threshold(5);

        let mut core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(FileStorage::new(dir3.path().to_str().unwrap()).unwrap()),
            Box::new(KeyValueStore::new()),
        );
        core3.set_snapshot_threshold(5);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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

        // Add 10 entries
        for i in 1..=10 {
            {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
            }
        }

        // Use heartbeats to replicate
        for round in 0..15 {
            let (_, _, _) = tokio::join!(
                server1.node.send_heartbeat(),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );

            let commit_idx = shared1.lock().await.commit_index;
            if commit_idx >= 11 {
                println!("Phase 1 committed after {} rounds", round + 1);
                break;
            }
        }

        // Verify state
        {
            let core = shared1.lock().await;
            println!("Leader: commit={}, snapshot={}, log.len={}",
                     core.commit_index, core.snapshot_last_index, core.log.len());
            assert!(core.commit_index >= 11, "Should commit all entries");
        }

        // Add more entries after snapshot
        for i in 11..=15 {
            {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
            }
        }

        // Replicate with heartbeats
        for round in 0..15 {
            let (_, _, _) = tokio::join!(
                server1.node.send_heartbeat(),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );

            let commit_idx = shared1.lock().await.commit_index;
            let last_log = shared1.lock().await.last_log_index();
            if commit_idx >= last_log {
                println!("Phase 2 committed after {} rounds", round + 1);
                break;
            }
        }

        // Final verification
        {
            let core1 = shared1.lock().await;
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;

            let leader_last = core1.last_log_index();
            println!("Final: leader_last={}, commits=({},{},{})",
                     leader_last, core1.commit_index, core2.commit_index, core3.commit_index);

            assert_eq!(core2.last_log_index(), leader_last);
            assert_eq!(core3.last_log_index(), leader_last);
            assert_eq!(core1.commit_index, leader_last);
        }
    }

    /// Test where LEADER snapshots first, then sends entries to followers
    #[tokio::test(start_paused = true)]
    async fn test_leader_snapshots_before_followers() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::storage::memory::MemoryStorage;
        use crate::transport::inmemory::create_cluster_with_timeout;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Leader has LOW threshold (will snapshot at 10)
        let mut core1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core1.set_snapshot_threshold(10);

        // Followers have HIGH threshold (won't snapshot)
        let mut core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core2.set_snapshot_threshold(100);

        let mut core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core3.set_snapshot_threshold(100);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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

        // Add 9 entries (NOOP + 9 = 10 total, will trigger leader snapshot)
        for i in 1..=9 {
            {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
            }
        }

        // Replicate and commit - leader will snapshot
        for _ in 0..5 {
            let (_, _, _) = tokio::join!(
                server1.node.send_heartbeat(),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );
        }

        // Verify leader took snapshot but followers didn't
        {
            let core1 = shared1.lock().await;
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;

            println!("After 10 entries:");
            println!("  Leader: snapshot={}, snapshot_term={}, log.len={}",
                     core1.snapshot_last_index, core1.snapshot_last_term, core1.log.len());
            println!("  Follower2: snapshot={}, log.len={}", core2.snapshot_last_index, core2.log.len());
            println!("  Follower3: snapshot={}, log.len={}", core3.snapshot_last_index, core3.log.len());

            assert!(core1.snapshot_last_index >= 10, "Leader should have snapshot");
            assert_eq!(core2.snapshot_last_index, 0, "Follower 2 should NOT have snapshot");
            assert_eq!(core3.snapshot_last_index, 0, "Follower 3 should NOT have snapshot");
        }

        // Now add entry 11 - leader has snapshotted, followers haven't
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("SET k10 v10".to_string());
            println!("Leader appended entry {} (snapshot={}, log.len={})",
                     core.last_log_index(), core.snapshot_last_index, core.log.len());
        }

        // Send heartbeat - followers should receive entry 11
        let (_, _, _) = tokio::join!(
            server1.node.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Verify entry 11 was replicated
        {
            let core1 = shared1.lock().await;
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;

            println!("After entry 11:");
            println!("  Leader: last_log={}, commit={}", core1.last_log_index(), core1.commit_index);
            println!("  Follower2: last_log={}, commit={}", core2.last_log_index(), core2.commit_index);
            println!("  Follower3: last_log={}, commit={}", core3.last_log_index(), core3.commit_index);

            assert!(core1.commit_index >= 11, "Entry 11 should be committed");
            assert_eq!(core2.last_log_index(), 11, "Follower 2 should have entry 11");
            assert_eq!(core3.last_log_index(), 11, "Follower 3 should have entry 11");
        }
    }

    /// Test the specific case where followers snapshot BEFORE leader
    #[tokio::test(start_paused = true)]
    async fn test_followers_snapshot_before_leader() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::storage::memory::MemoryStorage;
        use crate::transport::inmemory::create_cluster_with_timeout;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Leader has HIGH threshold (won't snapshot)
        let mut core1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core1.set_snapshot_threshold(100); // Won't trigger

        // Followers have LOW threshold (will snapshot at 10)
        let mut core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core2.set_snapshot_threshold(10);

        let mut core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core3.set_snapshot_threshold(10);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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

        // Add 10 entries (NOOP + 9 = 10 total, will trigger follower snapshot)
        for i in 1..=9 {
            {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
            }
        }

        // Replicate and commit - followers will snapshot
        for _ in 0..5 {
            let (_, _, _) = tokio::join!(
                server1.node.send_heartbeat(),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );
        }

        // Verify followers took snapshot but leader didn't
        {
            let core1 = shared1.lock().await;
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;

            println!("After 10 entries:");
            println!("  Leader: snapshot_last_index={}, log.len={}", core1.snapshot_last_index, core1.log.len());
            println!("  Follower2: snapshot_last_index={}, log.len={}", core2.snapshot_last_index, core2.log.len());
            println!("  Follower3: snapshot_last_index={}, log.len={}", core3.snapshot_last_index, core3.log.len());

            assert_eq!(core1.snapshot_last_index, 0, "Leader should NOT have snapshot");
            assert!(core2.snapshot_last_index >= 10, "Follower 2 should have snapshot");
            assert!(core3.snapshot_last_index >= 10, "Follower 3 should have snapshot");
        }

        // Now add entry 11 - this is where the bug occurs
        {
            let mut core = shared1.lock().await;
            core.append_log_entry("SET k10 v10".to_string());
            println!("Leader appended entry {}", core.last_log_index());
        }

        // Send heartbeat - followers should receive entry 11
        let (_, _, _) = tokio::join!(
            server1.node.send_heartbeat(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );

        // Verify entry 11 was replicated
        {
            let core1 = shared1.lock().await;
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;

            println!("After entry 11:");
            println!("  Leader: last_log={}, commit={}", core1.last_log_index(), core1.commit_index);
            println!("  Follower2: last_log={}, commit={}", core2.last_log_index(), core2.commit_index);
            println!("  Follower3: last_log={}, commit={}", core3.last_log_index(), core3.commit_index);

            // Entry 11 should be committed
            assert!(core1.commit_index >= 11, "Entry 11 should be committed");
            assert_eq!(core2.last_log_index(), 11, "Follower 2 should have entry 11");
            assert_eq!(core3.last_log_index(), 11, "Follower 3 should have entry 11");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_replication_continues_after_snapshot() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::storage::memory::MemoryStorage;
        use crate::transport::inmemory::create_cluster_with_timeout;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        // Create cores with KeyValueStore state machine and low snapshot threshold
        let mut core1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core1.set_snapshot_threshold(5); // Trigger snapshot after 5 entries

        let mut core2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core2.set_snapshot_threshold(5);

        let mut core3 = RaftCore::new(
            3,
            vec![1, 2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        core3.set_snapshot_threshold(5);

        let transport1 = transports.remove(&1).unwrap();

        let (server1, shared1) = RaftServer::new(core1, transport1);
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

        // === Phase 1: Append entries until snapshot triggers ===
        // NOOP is entry 1, so we need 4 more entries to reach threshold of 5
        for i in 1..=6 {
            {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
            }
            let idx = i + 1; // Account for NOOP at index 1
            let (_, _, _) = tokio::join!(
                server1.node.replicate_to_peers(idx),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );
        }

        // Verify entries committed
        {
            let core = shared1.lock().await;
            assert!(core.commit_index >= 5, "Should have committed entries");
        }

        // Verify snapshot was taken on leader
        {
            let core = shared1.lock().await;
            assert!(core.snapshot_last_index > 0, "Leader should have taken snapshot");
            println!("Leader snapshot at index {}", core.snapshot_last_index);
        }

        // === Phase 2: Append more entries AFTER snapshot ===
        for i in 7..=10 {
            let idx = {
                let mut core = shared1.lock().await;
                core.append_log_entry(format!("SET k{} v{}", i, i));
                core.last_log_index()
            };
            let (_, _, _) = tokio::join!(
                server1.node.replicate_to_peers(idx),
                handle2.process_one_shared(&shared2),
                handle3.process_one_shared(&shared3),
            );
        }

        // Get final state
        let final_log_index = shared1.lock().await.last_log_index();

        // Verify post-snapshot entries were committed
        {
            let core = shared1.lock().await;
            assert!(
                core.commit_index >= final_log_index,
                "Should commit all entries after snapshot: commit_index={}, last_log_index={}",
                core.commit_index,
                final_log_index
            );
        }

        // Verify followers have the entries
        {
            let core2 = shared2.lock().await;
            let core3 = shared3.lock().await;

            // Followers should have entries after their snapshot point
            println!("Follower 2: snapshot_last_index={}, log.len={}",
                     core2.snapshot_last_index, core2.log.len());
            println!("Follower 3: snapshot_last_index={}, log.len={}",
                     core3.snapshot_last_index, core3.log.len());

            // Check that last_log_index matches leader
            let leader_last = shared1.lock().await.last_log_index();
            assert_eq!(core2.last_log_index(), leader_last,
                       "Follower 2 should have same last_log_index as leader");
            assert_eq!(core3.last_log_index(), leader_last,
                       "Follower 3 should have same last_log_index as leader");
        }
    }
}
