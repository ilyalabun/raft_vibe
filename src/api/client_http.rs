//! HTTP client API for Raft cluster
//!
//! Provides HTTP endpoints for external clients to interact with the Raft cluster:
//! - Submit commands to the leader
//! - Query cluster status and leader information

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::core::raft_core::{RaftCore, RaftState};
use crate::core::raft_server::{RaftError, RaftHandle};

/// Shared state for the client HTTP server
pub type SharedCore = Arc<Mutex<RaftCore>>;

/// State for client HTTP handlers - contains both handle for commands and core for queries
#[derive(Clone)]
pub struct ClientState {
    /// Handle for submitting commands (goes through full RaftServer flow)
    pub handle: RaftHandle,
    /// Shared core for reading status/leader info
    pub core: SharedCore,
}

/// Request body for submitting a command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRequest {
    /// The command to submit (e.g., "SET key value")
    pub command: String,
}

/// Response from a successful submit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResponse {
    /// Result from the state machine
    pub result: String,
}

/// Error response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Error message
    pub error: String,
    /// Leader hint (node ID) if known, for redirect
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader_hint: Option<u64>,
}

/// Response for leader query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderResponse {
    /// Current leader ID if known
    pub leader_id: Option<u64>,
    /// This node's ID
    pub node_id: u64,
    /// Whether this node is the leader
    pub is_leader: bool,
}

/// Response for status query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    /// This node's ID
    pub node_id: u64,
    /// Current state (Follower, Candidate, Leader)
    pub state: String,
    /// Current term
    pub term: u64,
    /// Current leader ID if known
    pub leader_id: Option<u64>,
    /// Commit index
    pub commit_index: u64,
    /// Last applied index
    pub last_applied: u64,
    /// Number of log entries
    pub log_length: u64,
}

/// Create an axum router for client HTTP API (simplified, core-only version for testing)
pub fn create_client_router(core: SharedCore) -> Router {
    Router::new()
        .route("/client/leader", get(handle_leader_core))
        .route("/client/status", get(handle_status_core))
        .with_state(core)
}

/// Create an axum router for client HTTP API with full RaftServer integration
pub fn create_client_router_full(handle: RaftHandle, core: SharedCore) -> Router {
    let state = ClientState { handle, core };
    Router::new()
        .route("/client/submit", post(handle_submit))
        .route("/client/leader", get(handle_leader))
        .route("/client/status", get(handle_status))
        .with_state(state)
}

/// Handle POST /client/submit - submit a command to the Raft cluster
/// This goes through the full RaftServer flow: append, replicate, commit, apply
pub async fn handle_submit(
    State(state): State<ClientState>,
    Json(request): Json<SubmitRequest>,
) -> Result<Json<SubmitResponse>, (StatusCode, Json<ErrorResponse>)> {
    match state.handle.submit(request.command).await {
        Ok(result) => Ok(Json(SubmitResponse { result })),
        Err(RaftError::NotLeader { leader_hint }) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Not the leader".to_string(),
                leader_hint,
            }),
        )),
        Err(RaftError::NotCommitted) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Entry not committed (couldn't reach majority)".to_string(),
                leader_hint: None,
            }),
        )),
        Err(RaftError::StateMachine(err)) => Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("State machine error: {}", err),
                leader_hint: None,
            }),
        )),
        Err(RaftError::Transport(_)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Transport error".to_string(),
                leader_hint: None,
            }),
        )),
    }
}

/// Handle GET /client/leader - get current leader information (full integration)
pub async fn handle_leader(State(state): State<ClientState>) -> Json<LeaderResponse> {
    let core = state.core.lock().await;
    Json(LeaderResponse {
        leader_id: core.current_leader,
        node_id: core.id,
        is_leader: core.state == RaftState::Leader,
    })
}

/// Handle GET /client/status - get node status (full integration)
pub async fn handle_status(State(state): State<ClientState>) -> Json<StatusResponse> {
    let core = state.core.lock().await;
    Json(StatusResponse {
        node_id: core.id,
        state: format!("{:?}", core.state),
        term: core.current_term,
        leader_id: core.current_leader,
        commit_index: core.commit_index,
        last_applied: core.last_applied,
        log_length: core.log.len() as u64,
    })
}

/// Handle GET /client/leader - core-only version for testing
async fn handle_leader_core(State(core): State<SharedCore>) -> Json<LeaderResponse> {
    let core = core.lock().await;
    Json(LeaderResponse {
        leader_id: core.current_leader,
        node_id: core.id,
        is_leader: core.state == RaftState::Leader,
    })
}

/// Handle GET /client/status - core-only version for testing
async fn handle_status_core(State(core): State<SharedCore>) -> Json<StatusResponse> {
    let core = core.lock().await;
    Json(StatusResponse {
        node_id: core.id,
        state: format!("{:?}", core.state),
        term: core.current_term,
        leader_id: core.current_leader,
        commit_index: core.commit_index,
        last_applied: core.last_applied,
        log_length: core.log.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::TestStateMachine;
    use crate::storage::memory::MemoryStorage;
    use crate::core::raft_server::RaftServer;
    use crate::transport::inmemory::create_cluster_with_timeout;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::time::Duration;
    use tower::util::ServiceExt;

    fn new_test_core(id: u64, peers: Vec<u64>) -> RaftCore {
        RaftCore::new(
            id,
            peers,
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        )
    }

    // === Core-only tests (leader/status endpoints) ===

    #[tokio::test]
    async fn test_leader_endpoint_follower() {
        let core = Arc::new(Mutex::new(new_test_core(1, vec![2, 3])));
        let app = create_client_router(core);

        let request = Request::builder()
            .method("GET")
            .uri("/client/leader")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: LeaderResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.node_id, 1);
        assert!(!result.is_leader);
        assert_eq!(result.leader_id, None);
    }

    #[tokio::test]
    async fn test_leader_endpoint_leader() {
        let mut core = new_test_core(1, vec![2, 3]);
        core.become_leader();
        let core = Arc::new(Mutex::new(core));
        let app = create_client_router(core);

        let request = Request::builder()
            .method("GET")
            .uri("/client/leader")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: LeaderResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.node_id, 1);
        assert!(result.is_leader);
        assert_eq!(result.leader_id, Some(1));
    }

    #[tokio::test]
    async fn test_status_endpoint() {
        let mut core = new_test_core(1, vec![2, 3]);
        core.current_term = 5;
        core.commit_index = 3;
        core.last_applied = 2;
        let core = Arc::new(Mutex::new(core));
        let app = create_client_router(core);

        let request = Request::builder()
            .method("GET")
            .uri("/client/status")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: StatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.node_id, 1);
        assert_eq!(result.state, "Follower");
        assert_eq!(result.term, 5);
        assert_eq!(result.commit_index, 3);
        assert_eq!(result.last_applied, 2);
    }

    // === Full integration tests (submit endpoint with RaftServer) ===

    #[tokio::test(start_paused = true)]
    async fn test_submit_not_leader_full() {
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, _handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let transport1 = transports.remove(&1).unwrap();

        let (server, shared1) = RaftServer::new(core1, transport1);
        let handle = server.start();

        // Node 1 is not leader, submit should fail
        let app = create_client_router_full(handle.clone(), shared1);

        let request = Request::builder()
            .method("POST")
            .uri("/client/submit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"command": "SET x=1"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error, "Not the leader");
    }

    #[tokio::test(start_paused = true)]
    async fn test_submit_as_leader_full() {
        let node_ids = vec![1, 2, 3];
        let timeout = Duration::from_millis(100);
        let (mut transports, mut handles) = create_cluster_with_timeout(&node_ids, Some(timeout));

        let core1 = new_test_core(1, vec![2, 3]);
        let core2 = new_test_core(2, vec![1, 3]);
        let core3 = new_test_core(3, vec![1, 2]);

        let transport1 = transports.remove(&1).unwrap();
        let transport2 = transports.remove(&2).unwrap();
        let transport3 = transports.remove(&3).unwrap();

        let mut handle2 = handles.remove(&2).unwrap();
        let mut handle3 = handles.remove(&3).unwrap();

        // Set up servers (but don't start them yet)
        let (server1, shared1) = RaftServer::new(core1, transport1);
        let (_server2, shared2) = RaftServer::new(core2, transport2);
        let (_server3, shared3) = RaftServer::new(core3, transport3);

        // Win election manually before starting server loop
        server1.start_election().await;
        let (_, _, _) = tokio::join!(
            server1.request_votes(),
            handle2.process_one_shared(&shared2),
            handle3.process_one_shared(&shared3),
        );
        assert_eq!(server1.state().await, RaftState::Leader);

        // Now start the server loop
        let raft_handle1 = server1.start();

        // Create client router with full integration
        let app = create_client_router_full(raft_handle1, shared1.clone());

        // Submit a command via HTTP
        let request = Request::builder()
            .method("POST")
            .uri("/client/submit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"command": "SET x=42"}"#))
            .unwrap();

        // Process the submit while handling peer requests
        let shared2_clone = shared2.clone();
        let shared3_clone = shared3.clone();

        // Spawn peer handlers (they'll process the replication requests)
        tokio::spawn(async move {
            tokio::join!(
                handle2.process_one_shared(&shared2_clone),
                handle3.process_one_shared(&shared3_clone),
            );
        });

        // Spawn the HTTP request
        let response_task = tokio::spawn(async move {
            app.oneshot(request).await
        });

        // Advance time to let all tasks make progress
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }

        let response = response_task.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: SubmitResponse = serde_json::from_slice(&body).unwrap();
        // TestStateMachine returns empty string on success
        assert_eq!(result.result, "");

        // Verify entry was committed (NOOP at index 1 + command at index 2)
        assert_eq!(shared1.lock().await.commit_index, 2);
        assert_eq!(shared1.lock().await.last_applied, 2);
    }
}
