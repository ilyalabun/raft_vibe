//! RaftServer - High-level Raft server for handling client commands

use tokio::sync::{mpsc, oneshot};

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
}

impl<T: Transport + 'static> RaftServer<T> {
    /// Create a new RaftServer
    /// Returns the server and a shared reference to the core for incoming RPC handling
    pub fn new(core: RaftCore, transport: T) -> (Self, SharedCore) {
        let (command_tx, command_rx) = mpsc::channel(32);
        let node = RaftNode::new(core, transport);
        let shared_core = node.shared_core();
        let server = Self {
            node,
            command_rx,
            command_tx,
        };
        (server, shared_core)
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
        loop {
            match self.command_rx.recv().await {
                Some(Command::Submit { command, reply }) => {
                    let result = self.handle_submit(command).await;
                    let _ = reply.send(result);
                }
                None => break, // Channel closed, shutdown
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
    use tokio::sync::Mutex;
    use crate::transport_inmemory::create_cluster;

    #[tokio::test]
    async fn test_server_not_leader() {
        let node_ids = vec![1, 2, 3];
        let (mut transports, _handles) = create_cluster(&node_ids);

        let core1 = RaftCore::new(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();

        let (server, _shared_core) = RaftServer::new(core1, transport1);
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

        let (server1, _shared1) = RaftServer::new(core1, transport1);
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

        let (server1, shared1) = RaftServer::new(core1, transport1);
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
}
