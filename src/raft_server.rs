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
    /// This node is not the leader
    NotLeader,
    /// Transport error occurred
    Transport(TransportError),
}

/// Command sent to the RaftServer from clients
enum Command {
    /// Submit a client command to be replicated
    Submit {
        command: String,
        reply: oneshot::Sender<Result<u64, RaftError>>,
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
    /// Returns the log index if successful
    pub async fn submit(&self, command: String) -> Result<u64, RaftError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(Command::Submit {
                command,
                reply: reply_tx,
            })
            .await
            .map_err(|_| RaftError::NotLeader)?;

        reply_rx.await.map_err(|_| RaftError::NotLeader)?
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
    async fn handle_submit(&self, command: String) -> Result<u64, RaftError> {
        let shared_core = self.node.shared_core();
        let entry_index = {
            let mut core = shared_core.lock().await;

            // Only leaders can accept commands
            if core.state != RaftState::Leader {
                return Err(RaftError::NotLeader);
            }

            // Append to local log
            let entry = core.append_log_entry(command).ok_or(RaftError::NotLeader)?;
            entry.index
        };

        // Replicate to all peers
        self.node.replicate_to_peers(entry_index).await;

        // Return the index
        Ok(entry_index)
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
    use crate::transport_inmemory::create_cluster;

    #[tokio::test]
    async fn test_server_not_leader() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, _handles) = create_cluster(&node_ids);

        let core1 = RaftCore::new(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();

        let (server, _shared_core, _event_tx) = RaftServer::new(core1, transport1);
        let handle = server.start();

        // Node is not leader, should fail
        let result = handle.submit("SET x=1".to_string()).await;
        assert!(matches!(result, Err(RaftError::NotLeader)));
    }

    #[tokio::test]
    async fn test_election_via_server() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, mut handles) = create_cluster(&node_ids);

        let core1 = RaftCore::new(1, vec![2, 3]);
        let core2 = RaftCore::new(2, vec![1, 3]);
        let core3 = RaftCore::new(3, vec![1, 2]);

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

        let core1 = RaftCore::new(1, vec![2, 3]);
        let core2 = RaftCore::new(2, vec![1, 3]);
        let core3 = RaftCore::new(3, vec![1, 2]);

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

        let core1 = RaftCore::new(1, vec![2, 3]);
        let core2 = RaftCore::new(2, vec![1, 3]);
        let core3 = RaftCore::new(3, vec![1, 2]);

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

        let core1 = RaftCore::new(1, vec![2, 3]);
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
}
