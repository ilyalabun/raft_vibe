//! HTTP test client for chaos testing
//!
//! Wraps reqwest HTTP client to record operations with timestamps to a shared History.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::history::{ClientId, History, OpKind, OpResult, Operation, Timestamp};

/// Error types for client operations
#[derive(Debug, Clone)]
pub enum ClientError {
    /// Not the leader, with optional leader hint
    NotLeader { leader_hint: Option<u64> },
    /// Request timed out
    Timeout,
    /// Network or other error
    NetworkError(String),
}

/// Response for GET /kv/:key
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KvGetResponse {
    key: String,
    value: Option<String>,
}

/// Request body for POST /kv/:key
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KvSetRequest {
    value: String,
}

/// Response for POST /kv/:key
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KvMutationResponse {
    key: String,
    success: bool,
}

/// Error response from the server
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ErrorResponse {
    error: String,
    leader_hint: Option<u64>,
}

/// HTTP test client that records operations to a shared History
pub struct TestClient {
    /// Unique client identifier
    pub(crate) client_id: ClientId,
    /// Node addresses for failover (format: "host:port")
    targets: Vec<String>,
    /// HTTP client
    http: reqwest::Client,
    /// Shared history for recording operations
    history: Arc<Mutex<History>>,
    /// Shared operation ID counter
    next_op_id: Arc<AtomicU64>,
    /// Maximum retries for leader redirection
    max_retries: usize,
}

impl TestClient {
    /// Create a new test client
    pub fn new(
        client_id: ClientId,
        targets: Vec<String>,
        history: Arc<Mutex<History>>,
        op_counter: Arc<AtomicU64>,
        request_timeout: Duration,
    ) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .timeout(request_timeout)
            .build()
            .expect("Failed to create HTTP client");

        TestClient {
            client_id,
            targets,
            http,
            history,
            next_op_id: op_counter,
            max_retries: 10,
        }
    }

    /// Get the next operation ID
    fn next_id(&self) -> u64 {
        self.next_op_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Get current timestamp from the shared history
    fn now(&self) -> Timestamp {
        self.history.lock().now()
    }

    /// Record an operation to the shared history
    fn record(&self, op: Operation) {
        self.history.lock().add(op);
    }

    /// Read a key - records operation to history with timestamps
    pub async fn read(&self, key: &str) -> Result<Option<String>, ClientError> {
        let op_id = self.next_id();
        let invoke_ts = self.now();

        let result = self.do_read(key).await;

        let complete_ts = self.now();

        let op_result = match &result {
            Ok(value) => OpResult::ReadOk(value.clone()),
            Err(e) => OpResult::Error(format!("{:?}", e)),
        };

        self.record(Operation::new(
            op_id,
            self.client_id,
            key.to_string(),
            OpKind::Read,
            invoke_ts,
            complete_ts,
            op_result,
        ));

        result
    }

    /// Write a key - records operation to history with timestamps
    pub async fn write(&self, key: &str, value: &str) -> Result<(), ClientError> {
        let op_id = self.next_id();
        let invoke_ts = self.now();

        let result = self.do_write(key, value).await;

        let complete_ts = self.now();

        let op_result = match &result {
            Ok(()) => OpResult::WriteOk,
            Err(e) => OpResult::Error(format!("{:?}", e)),
        };

        self.record(Operation::new(
            op_id,
            self.client_id,
            key.to_string(),
            OpKind::Write {
                value: value.to_string(),
            },
            invoke_ts,
            complete_ts,
            op_result,
        ));

        result
    }

    /// Internal read implementation with leader redirection
    async fn do_read(&self, key: &str) -> Result<Option<String>, ClientError> {
        let mut current_target_idx = 0;
        let mut retries = 0;

        loop {
            let target = &self.targets[current_target_idx];
            let url = format!("http://{}/kv/{}", target, key);

            match self.http.get(&url).send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        let resp: KvGetResponse = response
                            .json()
                            .await
                            .map_err(|e| ClientError::NetworkError(e.to_string()))?;
                        return Ok(resp.value);
                    } else if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                        // Not leader - try to follow hint or try next node
                        let error: ErrorResponse = response
                            .json()
                            .await
                            .map_err(|e| ClientError::NetworkError(e.to_string()))?;

                        retries += 1;
                        if retries >= self.max_retries {
                            return Err(ClientError::NotLeader {
                                leader_hint: error.leader_hint,
                            });
                        }

                        // Try next target (round-robin)
                        current_target_idx = (current_target_idx + 1) % self.targets.len();
                        // Small delay before retry
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    } else {
                        return Err(ClientError::NetworkError(format!(
                            "Unexpected status: {}",
                            response.status()
                        )));
                    }
                }
                Err(e) => {
                    if e.is_timeout() {
                        return Err(ClientError::Timeout);
                    }
                    // Network error - try next node
                    retries += 1;
                    if retries >= self.max_retries {
                        return Err(ClientError::NetworkError(e.to_string()));
                    }
                    current_target_idx = (current_target_idx + 1) % self.targets.len();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    /// Internal write implementation with leader redirection
    async fn do_write(&self, key: &str, value: &str) -> Result<(), ClientError> {
        let mut current_target_idx = 0;
        let mut retries = 0;

        loop {
            let target = &self.targets[current_target_idx];
            let url = format!("http://{}/kv/{}", target, key);

            let body = KvSetRequest {
                value: value.to_string(),
            };

            match self.http.post(&url).json(&body).send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        let _resp: KvMutationResponse = response
                            .json()
                            .await
                            .map_err(|e| ClientError::NetworkError(e.to_string()))?;
                        return Ok(());
                    } else if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                        // Not leader - try to follow hint or try next node
                        let error: ErrorResponse = response
                            .json()
                            .await
                            .map_err(|e| ClientError::NetworkError(e.to_string()))?;

                        retries += 1;
                        if retries >= self.max_retries {
                            return Err(ClientError::NotLeader {
                                leader_hint: error.leader_hint,
                            });
                        }

                        // Try next target (round-robin)
                        current_target_idx = (current_target_idx + 1) % self.targets.len();
                        // Small delay before retry
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    } else {
                        return Err(ClientError::NetworkError(format!(
                            "Unexpected status: {}",
                            response.status()
                        )));
                    }
                }
                Err(e) => {
                    if e.is_timeout() {
                        return Err(ClientError::Timeout);
                    }
                    // Network error - try next node
                    retries += 1;
                    if retries >= self.max_retries {
                        return Err(ClientError::NetworkError(e.to_string()));
                    }
                    current_target_idx = (current_target_idx + 1) % self.targets.len();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let history = Arc::new(Mutex::new(History::new()));
        let counter = Arc::new(AtomicU64::new(1));
        let targets = vec!["127.0.0.1:8080".to_string()];

        let client = TestClient::new(ClientId::new(1), targets, history, counter, Duration::from_secs(2));
        assert_eq!(client.client_id, ClientId::new(1));
        assert_eq!(client.targets.len(), 1);
    }

    #[test]
    fn test_op_id_generation() {
        let history = Arc::new(Mutex::new(History::new()));
        let counter = Arc::new(AtomicU64::new(1));
        let targets = vec!["127.0.0.1:8080".to_string()];

        let client = TestClient::new(ClientId::new(1), targets, history, counter.clone(), Duration::from_secs(2));

        assert_eq!(client.next_id(), 1);
        assert_eq!(client.next_id(), 2);
        assert_eq!(client.next_id(), 3);

        // Counter is shared
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }
}
