//! Linearizability tests against a real Raft cluster.
//!
//! These tests spin up an actual 3-node cluster and verify that concurrent
//! operations satisfy linearizability using the WGL checker from chaos-test.

use std::time::Duration;

use chaos_test::{run_test, TestConfig};
use raft_vibe::testing::TestCluster;

/// Test that a healthy cluster passes linearizability checks.
#[tokio::test]
async fn test_healthy_cluster() {
    let cluster = TestCluster::new().await;

    let leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    // Run linearizability test with 5 clients, 50 ops each
    let config = TestConfig {
        node_addresses: cluster.all_addr_strings(),
        client_count: 5,
        ops_per_client: 50,
        keys: vec!["x".to_string()],
        write_ratio: 0.5,
    };

    let result = run_test(config).await;

    println!(
        "Total ops: {}, Successful: {}, Duration: {:?}",
        result.total_ops, result.successful_ops, result.duration
    );
    println!(
        "Linearizable: {}, Error: {:?}",
        result.check.is_linearizable, result.check.error
    );

    assert!(
        result.check.is_linearizable,
        "Healthy cluster should be linearizable: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > 0,
        "Should have successful operations"
    );

    cluster.shutdown().await;
}

/// Test with higher concurrency to stress test linearizability.
#[tokio::test]
async fn test_high_concurrency() {
    let cluster = TestCluster::new().await;

    let leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    // Higher concurrency: 10 clients, 20 ops each
    let config = TestConfig {
        node_addresses: cluster.all_addr_strings(),
        client_count: 10,
        ops_per_client: 20,
        keys: vec!["stress".to_string()],
        write_ratio: 0.5,
    };

    let result = run_test(config).await;

    println!(
        "High concurrency: {} total ops, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "High concurrency should still be linearizable: {:?}",
        result.check.error
    );

    cluster.shutdown().await;
}

/// Test with mostly writes to stress write ordering.
#[tokio::test]
async fn test_write_heavy() {
    let cluster = TestCluster::new().await;

    let leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    // Write-heavy: 80% writes
    let config = TestConfig {
        node_addresses: cluster.all_addr_strings(),
        client_count: 5,
        ops_per_client: 30,
        keys: vec!["writeheavy".to_string()],
        write_ratio: 0.8,
    };

    let result = run_test(config).await;

    println!(
        "Write-heavy: {} total ops, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Write-heavy workload should be linearizable: {:?}",
        result.check.error
    );

    cluster.shutdown().await;
}

/// Test with mostly reads to stress read consistency.
#[tokio::test]
async fn test_read_heavy() {
    let cluster = TestCluster::new().await;

    let leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    // Read-heavy: 80% reads
    let config = TestConfig {
        node_addresses: cluster.all_addr_strings(),
        client_count: 5,
        ops_per_client: 30,
        keys: vec!["readheavy".to_string()],
        write_ratio: 0.2,
    };

    let result = run_test(config).await;

    println!(
        "Read-heavy: {} total ops, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Read-heavy workload should be linearizable: {:?}",
        result.check.error
    );

    cluster.shutdown().await;
}

/// Test with multiple independent keys.
#[tokio::test]
async fn test_multiple_keys() {
    let cluster = TestCluster::new().await;

    let leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    // Multiple keys: each key is verified independently
    let config = TestConfig {
        node_addresses: cluster.all_addr_strings(),
        client_count: 5,
        ops_per_client: 30,
        keys: vec![
            "key1".to_string(),
            "key2".to_string(),
            "key3".to_string(),
        ],
        write_ratio: 0.5,
    };

    let result = run_test(config).await;

    println!(
        "Multiple keys: {} total ops, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Multiple keys should be linearizable: {:?}",
        result.check.error
    );

    cluster.shutdown().await;
}
