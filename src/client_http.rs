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

use crate::raft_core::{RaftCore, RaftState};

/// Shared state for the client HTTP server
pub type SharedCore = Arc<Mutex<RaftCore>>;

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

/// Create an axum router for client HTTP API
pub fn create_client_router(core: SharedCore) -> Router {
    Router::new()
        .route("/client/submit", post(handle_submit))
        .route("/client/leader", get(handle_leader))
        .route("/client/status", get(handle_status))
        .with_state(core)
}

/// Handle POST /client/submit - submit a command to the Raft cluster
/// Note: This is a simplified version that only appends to the log.
/// In a full implementation, we'd wait for commit via RaftServer.
async fn handle_submit(
    State(core): State<SharedCore>,
    Json(request): Json<SubmitRequest>,
) -> Result<Json<SubmitResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut core = core.lock().await;

    // Only leaders can accept commands
    if core.state != RaftState::Leader {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Not the leader".to_string(),
                leader_hint: core.current_leader,
            }),
        ));
    }

    // Append to local log
    match core.append_log_entry(request.command) {
        Some(_entry) => {
            // Note: In production, we'd wait for replication and return the actual
            // state machine result. For now, we return a placeholder indicating
            // the entry was accepted.
            Ok(Json(SubmitResponse {
                result: "Entry accepted (pending replication)".to_string(),
            }))
        }
        None => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to append log entry".to_string(),
                leader_hint: None,
            }),
        )),
    }
}

/// Handle GET /client/leader - get current leader information
async fn handle_leader(State(core): State<SharedCore>) -> Json<LeaderResponse> {
    let core = core.lock().await;
    Json(LeaderResponse {
        leader_id: core.current_leader,
        node_id: core.id,
        is_leader: core.state == RaftState::Leader,
    })
}

/// Handle GET /client/status - get node status
async fn handle_status(State(core): State<SharedCore>) -> Json<StatusResponse> {
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
    use crate::storage_memory::MemoryStorage;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn new_test_core(id: u64, peers: Vec<u64>) -> RaftCore {
        RaftCore::new(
            id,
            peers,
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        )
    }

    #[tokio::test]
    async fn test_submit_not_leader() {
        let core = Arc::new(Mutex::new(new_test_core(1, vec![2, 3])));
        let app = create_client_router(core);

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
        assert_eq!(error.leader_hint, None);
    }

    #[tokio::test]
    async fn test_submit_as_leader() {
        let mut core = new_test_core(1, vec![2, 3]);
        core.become_leader();
        let core = Arc::new(Mutex::new(core));
        let app = create_client_router(core);

        let request = Request::builder()
            .method("POST")
            .uri("/client/submit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"command": "SET x=1"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: SubmitResponse = serde_json::from_slice(&body).unwrap();
        assert!(result.result.contains("accepted"));
    }

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

    #[tokio::test]
    async fn test_submit_with_leader_hint() {
        // Simulate a follower that knows who the leader is
        let mut core = new_test_core(1, vec![2, 3]);
        core.current_leader = Some(2); // We know node 2 is the leader
        let core = Arc::new(Mutex::new(core));
        let app = create_client_router(core);

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
        assert_eq!(error.leader_hint, Some(2)); // Should hint at node 2
    }
}
