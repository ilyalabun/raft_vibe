//! HTTP transport implementation for Raft RPC
//!
//! Uses axum for the server and reqwest for the client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    extract::State,
    http::StatusCode,
    routing::post,
    Json, Router,
};
use tokio::sync::Mutex;

use crate::raft_core::{
    AppendEntriesArgs, AppendEntriesResult, RaftCore, RequestVoteArgs, RequestVoteResult,
};
use crate::transport::{Transport, TransportError};

/// HTTP transport for Raft RPC communication
pub struct HttpTransport {
    /// Map of node ID to address (e.g., "127.0.0.1:8001")
    peers: HashMap<u64, String>,
    /// HTTP client with timeout
    client: reqwest::Client,
}

impl HttpTransport {
    /// Create a new HTTP transport with peer addresses
    pub fn new(peers: HashMap<u64, String>, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("Failed to create HTTP client");

        HttpTransport { peers, client }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request_vote(
        &self,
        target: u64,
        args: RequestVoteArgs,
    ) -> Result<RequestVoteResult, TransportError> {
        let addr = self.peers.get(&target).ok_or(TransportError::NodeNotFound)?;
        let url = format!("http://{}/raft/request_vote", addr);

        let response = self
            .client
            .post(&url)
            .json(&args)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::ConnectionFailed
                }
            })?;

        response
            .json::<RequestVoteResult>()
            .await
            .map_err(|_| TransportError::ConnectionFailed)
    }

    async fn append_entries(
        &self,
        target: u64,
        args: AppendEntriesArgs,
    ) -> Result<AppendEntriesResult, TransportError> {
        let addr = self.peers.get(&target).ok_or(TransportError::NodeNotFound)?;
        let url = format!("http://{}/raft/append_entries", addr);

        let response = self
            .client
            .post(&url)
            .json(&args)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::ConnectionFailed
                }
            })?;

        response
            .json::<AppendEntriesResult>()
            .await
            .map_err(|_| TransportError::ConnectionFailed)
    }
}

/// Shared state for the HTTP server
pub type SharedCore = Arc<Mutex<RaftCore>>;

/// Create an axum router for handling Raft RPCs
pub fn create_router(core: SharedCore) -> Router {
    Router::new()
        .route("/raft/request_vote", post(handle_request_vote))
        .route("/raft/append_entries", post(handle_append_entries))
        .with_state(core)
}

async fn handle_request_vote(
    State(core): State<SharedCore>,
    Json(args): Json<RequestVoteArgs>,
) -> Json<RequestVoteResult> {
    let mut core = core.lock().await;
    let result = core.handle_request_vote(&args);
    Json(result)
}

async fn handle_append_entries(
    State(core): State<SharedCore>,
    Json(args): Json<AppendEntriesArgs>,
) -> (StatusCode, Json<AppendEntriesResult>) {
    let mut core = core.lock().await;
    let output = core.handle_append_entries(&args);
    // Note: We're ignoring the RaftEvent here for now
    // In a full implementation, we'd signal the server to reset election timeout
    (StatusCode::OK, Json(output.result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::TestStateMachine;
    use crate::storage_memory::MemoryStorage;
    use tokio::net::TcpListener;

    fn new_test_core(id: u64, peers: Vec<u64>) -> RaftCore {
        RaftCore::new(
            id,
            peers,
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        )
    }

    #[tokio::test]
    async fn test_http_request_vote() {
        // Start server for node 2
        let core2 = Arc::new(Mutex::new(new_test_core(2, vec![1, 3])));
        let router = create_router(core2.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Create transport for node 1
        let mut peers = HashMap::new();
        peers.insert(2, addr.to_string());
        let transport = HttpTransport::new(peers, Duration::from_secs(5));

        // Node 1 requests vote from node 2
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 1,
            last_log_index: 0,
            last_log_term: 0,
        };

        let result = transport.request_vote(2, args).await.unwrap();

        assert!(result.vote_granted);
        assert_eq!(result.term, 1);
    }

    #[tokio::test]
    async fn test_http_append_entries() {
        // Start server for node 2
        let core2 = Arc::new(Mutex::new(new_test_core(2, vec![1, 3])));
        let router = create_router(core2.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        // Create transport for node 1
        let mut peers = HashMap::new();
        peers.insert(2, addr.to_string());
        let transport = HttpTransport::new(peers, Duration::from_secs(5));

        // Node 1 sends heartbeat to node 2
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 1,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };

        let result = transport.append_entries(2, args).await.unwrap();

        assert!(result.success);
        assert_eq!(result.term, 1);
    }

    #[tokio::test]
    async fn test_http_timeout() {
        // Create transport pointing to non-existent server
        let mut peers = HashMap::new();
        peers.insert(2, "127.0.0.1:59999".to_string());
        let transport = HttpTransport::new(peers, Duration::from_millis(100));

        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 1,
            last_log_index: 0,
            last_log_term: 0,
        };

        let result = transport.request_vote(2, args).await;

        assert!(matches!(
            result,
            Err(TransportError::ConnectionFailed) | Err(TransportError::Timeout)
        ));
    }

    #[tokio::test]
    async fn test_http_node_not_found() {
        let peers = HashMap::new(); // Empty - no peers configured
        let transport = HttpTransport::new(peers, Duration::from_secs(5));

        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 1,
            last_log_index: 0,
            last_log_term: 0,
        };

        let result = transport.request_vote(2, args).await;

        assert!(matches!(result, Err(TransportError::NodeNotFound)));
    }
}
