//! In-memory transport implementation for testing

use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

use crate::raft_core::{
    AppendEntriesArgs, AppendEntriesResult, RaftCore, RequestVoteArgs, RequestVoteResult,
};
use crate::transport::{Transport, TransportError};

/// Request types that can be sent to a node
pub(crate) enum Request {
    RequestVote {
        args: RequestVoteArgs,
        reply: oneshot::Sender<RequestVoteResult>,
    },
    AppendEntries {
        args: AppendEntriesArgs,
        reply: oneshot::Sender<AppendEntriesResult>,
    },
}

/// In-memory transport that uses channels for communication
pub struct InMemoryTransport {
    /// Senders to each node's request channel
    senders: HashMap<u64, mpsc::Sender<Request>>,
    /// Optional timeout for RPC calls
    timeout: Option<Duration>,
}

impl InMemoryTransport {
    /// Create a new in-memory transport with senders to all nodes (no timeout)
    pub fn new(senders: HashMap<u64, mpsc::Sender<Request>>) -> Self {
        Self { senders, timeout: None }
    }

    /// Create a new in-memory transport with a timeout
    pub fn with_timeout(senders: HashMap<u64, mpsc::Sender<Request>>, timeout: Duration) -> Self {
        Self { senders, timeout: Some(timeout) }
    }
}

#[async_trait]
impl Transport for InMemoryTransport {
    async fn request_vote(
        &self,
        target: u64,
        args: RequestVoteArgs,
    ) -> Result<RequestVoteResult, TransportError> {
        let sender = self.senders.get(&target).ok_or(TransportError::NodeNotFound)?;

        let (reply_tx, reply_rx) = oneshot::channel();
        sender
            .send(Request::RequestVote { args, reply: reply_tx })
            .await
            .map_err(|_| TransportError::ConnectionFailed)?;

        // Apply timeout if configured
        match self.timeout {
            Some(timeout) => {
                tokio::time::timeout(timeout, reply_rx)
                    .await
                    .map_err(|_| TransportError::Timeout)?
                    .map_err(|_| TransportError::ConnectionFailed)
            }
            None => reply_rx.await.map_err(|_| TransportError::ConnectionFailed),
        }
    }

    async fn append_entries(
        &self,
        target: u64,
        args: AppendEntriesArgs,
    ) -> Result<AppendEntriesResult, TransportError> {
        let sender = self.senders.get(&target).ok_or(TransportError::NodeNotFound)?;

        let (reply_tx, reply_rx) = oneshot::channel();
        sender
            .send(Request::AppendEntries { args, reply: reply_tx })
            .await
            .map_err(|_| TransportError::ConnectionFailed)?;

        // Apply timeout if configured
        match self.timeout {
            Some(timeout) => {
                tokio::time::timeout(timeout, reply_rx)
                    .await
                    .map_err(|_| TransportError::Timeout)?
                    .map_err(|_| TransportError::ConnectionFailed)
            }
            None => reply_rx.await.map_err(|_| TransportError::ConnectionFailed),
        }
    }
}

/// Handle for a node that processes incoming requests
pub struct NodeHandle {
    receiver: mpsc::Receiver<Request>,
}

impl NodeHandle {
    /// Process one incoming request using the given RaftCore
    pub async fn process_one(&mut self, node: &mut RaftCore) -> bool {
        match self.receiver.recv().await {
            Some(request) => {
                Self::handle_request(request, node);
                true
            }
            None => false,
        }
    }

    /// Process one request using a shared node (for use with RaftServer)
    /// This method receives the request first, then briefly locks to process
    pub async fn process_one_shared(
        &mut self,
        node: &std::sync::Arc<tokio::sync::Mutex<RaftCore>>,
    ) -> bool {
        match self.receiver.recv().await {
            Some(request) => {
                let mut n = node.lock().await;
                Self::handle_request(request, &mut n);
                true
            }
            None => false,
        }
    }

    fn handle_request(request: Request, node: &mut RaftCore) {
        match request {
            Request::RequestVote { args, reply } => {
                let result = node.handle_request_vote(&args);
                let _ = reply.send(result);
            }
            Request::AppendEntries { args, reply } => {
                let output = node.handle_append_entries(&args);
                // Transport only returns the result; events are handled locally
                let _ = reply.send(output.result);
            }
        }
    }
}

/// Create transports and handles for a cluster of nodes
pub fn create_cluster(node_ids: &[u64]) -> (HashMap<u64, InMemoryTransport>, HashMap<u64, NodeHandle>) {
    create_cluster_with_timeout(node_ids, None)
}

/// Create transports and handles for a cluster of nodes with optional timeout
pub fn create_cluster_with_timeout(
    node_ids: &[u64],
    timeout: Option<Duration>,
) -> (HashMap<u64, InMemoryTransport>, HashMap<u64, NodeHandle>) {
    let mut senders: HashMap<u64, mpsc::Sender<Request>> = HashMap::new();
    let mut handles: HashMap<u64, NodeHandle> = HashMap::new();

    // Create a channel for each node
    for &id in node_ids {
        let (tx, rx) = mpsc::channel(32);
        senders.insert(id, tx);
        handles.insert(id, NodeHandle { receiver: rx });
    }

    // Create a transport for each node with senders to all other nodes
    let mut transports: HashMap<u64, InMemoryTransport> = HashMap::new();
    for &id in node_ids {
        let other_senders: HashMap<u64, mpsc::Sender<Request>> = senders
            .iter()
            .filter(|(&k, _)| k != id)
            .map(|(&k, v)| (k, v.clone()))
            .collect();
        let transport = match timeout {
            Some(t) => InMemoryTransport::with_timeout(other_senders, t),
            None => InMemoryTransport::new(other_senders),
        };
        transports.insert(id, transport);
    }

    (transports, handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raft_core::RaftCore;

    #[tokio::test]
    async fn test_request_vote() {
        let node_ids = vec![1, 2, 3];
        let (transports, mut handles) = create_cluster(&node_ids);

        let mut node2 = RaftCore::new(2, vec![1, 3]);

        // Node 1 requests vote from node 2
        let transport1 = transports.get(&1).unwrap();
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 1,
            last_log_index: 0,
            last_log_term: 0,
        };

        let vote_future = transport1.request_vote(2, args);

        // Process the request on node 2
        let handle2 = handles.get_mut(&2).unwrap();
        let (result, _) = tokio::join!(vote_future, handle2.process_one(&mut node2));

        let result = result.unwrap();
        assert!(result.vote_granted);
        assert_eq!(result.term, 1);
        assert_eq!(node2.voted_for, Some(1));
    }

    #[tokio::test]
    async fn test_append_entries() {
        let node_ids = vec![1, 2, 3];
        let (transports, mut handles) = create_cluster(&node_ids);

        let mut node2 = RaftCore::new(2, vec![1, 3]);

        // Node 1 sends append entries to node 2
        let transport1 = transports.get(&1).unwrap();
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };

        let append_future = transport1.append_entries(2, args);

        // Process the request on node 2
        let handle2 = handles.get_mut(&2).unwrap();
        let (result, _) = tokio::join!(append_future, handle2.process_one(&mut node2));

        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.term, 1);
    }

    #[tokio::test]
    async fn test_node_not_found() {
        let node_ids = vec![1, 2];
        let (transports, _handles) = create_cluster(&node_ids);

        let transport1 = transports.get(&1).unwrap();
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 1,
            last_log_index: 0,
            last_log_term: 0,
        };

        // Try to send to non-existent node 99
        let result = transport1.request_vote(99, args).await;
        assert!(matches!(result, Err(TransportError::NodeNotFound)));
    }

    #[tokio::test]
    async fn test_election_via_transport() {
        let node_ids = vec![1, 2, 3];
        let (transports, mut handles) = create_cluster(&node_ids);

        let mut node1 = RaftCore::new(1, vec![2, 3]);
        let mut node2 = RaftCore::new(2, vec![1, 3]);
        let mut node3 = RaftCore::new(3, vec![1, 2]);

        // Node 1 starts election
        node1.start_election();

        let transport1 = transports.get(&1).unwrap();
        let args = RequestVoteArgs {
            term: node1.current_term,
            candidate_id: node1.id,
            last_log_index: node1.last_log_index(),
            last_log_term: node1.last_log_term(),
        };

        // Send vote requests to both peers concurrently
        let vote2_future = transport1.request_vote(2, args.clone());
        let vote3_future = transport1.request_vote(3, args);

        // Extract handles to avoid multiple mutable borrows
        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Process requests and collect results
        let (result2, result3, _, _) = tokio::join!(
            vote2_future,
            vote3_future,
            handle2.process_one(&mut node2),
            handle3.process_one(&mut node3),
        );

        // Process vote results
        let became_leader2 = node1.handle_request_vote_result(2, &result2.unwrap());
        let became_leader3 = node1.handle_request_vote_result(3, &result3.unwrap());

        // Node 1 should become leader after receiving majority
        assert!(became_leader2 || became_leader3);
        assert_eq!(node1.state, crate::raft_core::RaftState::Leader);
    }

    #[tokio::test(start_paused = true)]
    async fn test_request_vote_timeout() {
        let node_ids = vec![1, 2];
        let timeout = Duration::from_millis(100);
        let (transports, _handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let transport1 = transports.get(&1).unwrap();
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 1,
            last_log_index: 0,
            last_log_term: 0,
        };

        // Request vote but don't process on node 2 - should timeout
        let result = transport1.request_vote(2, args).await;

        assert!(matches!(result, Err(TransportError::Timeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_append_entries_timeout() {
        let node_ids = vec![1, 2];
        let timeout = Duration::from_millis(100);
        let (transports, _handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let transport1 = transports.get(&1).unwrap();
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };

        // Send but don't process - should timeout
        let result = transport1.append_entries(2, args).await;

        assert!(matches!(result, Err(TransportError::Timeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_mixed_responses_and_timeouts() {
        // One peer responds, one times out
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let mut node2 = RaftCore::new(2, vec![1, 3]);
        // Node 3 won't respond

        let transport1 = transports.get(&1).unwrap();
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 1,
            last_log_index: 0,
            last_log_term: 0,
        };

        let mut handle2 = handles.remove(&2).unwrap();

        // Send to both, only node 2 responds
        let vote2_future = transport1.request_vote(2, args.clone());
        let vote3_future = transport1.request_vote(3, args);

        let (result2, result3, _) = tokio::join!(
            vote2_future,
            vote3_future,
            handle2.process_one(&mut node2),
        );

        // Node 2 should succeed, node 3 should timeout
        assert!(result2.is_ok());
        assert!(result2.unwrap().vote_granted);
        assert!(matches!(result3, Err(TransportError::Timeout)));
    }
}
