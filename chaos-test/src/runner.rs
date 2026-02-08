//! Test runner for chaos testing
//!
//! Orchestrates test execution: spawns clients, runs operations, and checks linearizability.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::checker::{CheckResult, LinearizabilityChecker};
use crate::client::TestClient;
use crate::history::{ClientId, History};

/// Configuration for a linearizability test
#[derive(Clone, Debug)]
pub struct TestConfig {
    /// Node addresses (format: "host:port")
    pub node_addresses: Vec<String>,
    /// Number of concurrent clients
    pub client_count: usize,
    /// Number of operations per client
    pub ops_per_client: usize,
    /// Key to use for single-key register test
    pub key: String,
    /// Ratio of writes (0.0 = all reads, 1.0 = all writes)
    pub write_ratio: f64,
}

impl Default for TestConfig {
    fn default() -> Self {
        TestConfig {
            node_addresses: vec![
                "127.0.0.1:8081".to_string(),
                "127.0.0.1:8082".to_string(),
                "127.0.0.1:8083".to_string(),
            ],
            client_count: 5,
            ops_per_client: 50,
            key: "x".to_string(),
            write_ratio: 0.5,
        }
    }
}

/// Result of a linearizability test
#[derive(Debug)]
pub struct TestResult {
    /// Linearizability check result
    pub check: CheckResult,
    /// Total number of operations attempted
    pub total_ops: usize,
    /// Total number of successful operations
    pub successful_ops: usize,
    /// Test duration
    pub duration: Duration,
    /// Full history for debugging
    pub history: History,
}

/// Run a linearizability test against a cluster
///
/// # Example
///
/// ```ignore
/// use chaos_test::{run_test, TestConfig};
///
/// let config = TestConfig {
///     node_addresses: vec!["127.0.0.1:8081".to_string()],
///     client_count: 3,
///     ops_per_client: 20,
///     key: "test_key".to_string(),
///     write_ratio: 0.5,
/// };
///
/// let result = run_test(config).await;
/// assert!(result.check.is_linearizable);
/// ```
pub async fn run_test(config: TestConfig) -> TestResult {
    let start = Instant::now();

    // Create shared history and operation counter
    let history = Arc::new(Mutex::new(History::new()));
    let op_counter = Arc::new(AtomicU64::new(1));

    // Spawn client tasks
    let mut handles = Vec::new();

    for i in 0..config.client_count {
        let client = TestClient::new(
            ClientId::new(i as u64),
            config.node_addresses.clone(),
            history.clone(),
            op_counter.clone(),
        );

        let key = config.key.clone();
        let ops = config.ops_per_client;
        let write_ratio = config.write_ratio;

        handles.push(tokio::spawn(async move {
            run_client_workload(client, &key, ops, write_ratio).await
        }));
    }

    // Wait for all clients to complete
    for handle in handles {
        let _ = handle.await;
    }

    let duration = start.elapsed();

    // Extract history for checking
    let final_history = history.lock().clone();
    let total_ops = final_history.len();
    let successful_ops = final_history.successful_ops().len();

    // Run linearizability checker
    let check = LinearizabilityChecker::check(&final_history);

    TestResult {
        check,
        total_ops,
        successful_ops,
        duration,
        history: final_history,
    }
}

/// Run workload for a single client
async fn run_client_workload(client: TestClient, key: &str, ops: usize, write_ratio: f64) {
    // Use StdRng which is Send (unlike ThreadRng)
    let mut rng = StdRng::from_os_rng();
    let mut value_counter = 0u64;

    for _ in 0..ops {
        let is_write: f64 = rng.random();
        let is_write = is_write < write_ratio;

        if is_write {
            // Generate unique value for this client's writes
            value_counter += 1;
            let value = format!("v{}", value_counter);
            let _ = client.write(key, &value).await;
        } else {
            let _ = client.read(key).await;
        }

        // Small random delay between operations to add variety
        let delay_ms: u64 = rng.random_range(0..10);
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = TestConfig::default();
        assert_eq!(config.client_count, 5);
        assert_eq!(config.ops_per_client, 50);
        assert_eq!(config.key, "x");
        assert!((config.write_ratio - 0.5).abs() < f64::EPSILON);
    }
}
