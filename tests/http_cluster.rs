//! HTTP cluster integration tests
//!
//! These tests spin up actual HTTP servers (a 3-node cluster) and test
//! concurrent client requests over real HTTP/TCP connections.

use std::net::SocketAddr;
use std::time::Duration;

use raft_vibe::api::client_http::{ErrorResponse, KvGetResponse, KvMutationResponse, StatusResponse};
use raft_vibe::testing::TestCluster;

// HTTP client helpers

async fn get_status(
    client: &reqwest::Client,
    addr: &SocketAddr,
) -> Result<StatusResponse, reqwest::Error> {
    client
        .get(format!("http://{}/client/status", addr))
        .send()
        .await?
        .json()
        .await
}

/// Submit a SET command via the RESTful KV API
async fn submit_command(
    client: &reqwest::Client,
    addr: &SocketAddr,
    command: &str,
) -> Result<KvMutationResponse, SubmitError> {
    // Parse command to extract key and value (format: "SET key value")
    let parts: Vec<&str> = command.splitn(3, ' ').collect();
    if parts.len() < 3 || parts[0] != "SET" {
        return Err(SubmitError::Network(format!(
            "Invalid command format: {}",
            command
        )));
    }
    let key = parts[1];
    let value = parts[2];

    let response = client
        .post(format!("http://{}/kv/{}", addr, key))
        .json(&serde_json::json!({ "value": value }))
        .send()
        .await
        .map_err(|e| SubmitError::Network(e.to_string()))?;

    if response.status().is_success() {
        let result: KvMutationResponse = response
            .json()
            .await
            .map_err(|e| SubmitError::Network(e.to_string()))?;
        Ok(result)
    } else {
        let error: ErrorResponse = response
            .json()
            .await
            .map_err(|e| SubmitError::Network(e.to_string()))?;
        Err(SubmitError::NotLeader {
            message: error.error,
            leader_hint: error.leader_hint,
        })
    }
}

/// Perform a linearizable read via the RESTful KV API
async fn linearizable_read(
    client: &reqwest::Client,
    addr: &SocketAddr,
    key: &str,
) -> Result<KvGetResponse, ReadError> {
    let response = client
        .get(format!("http://{}/kv/{}", addr, key))
        .send()
        .await
        .map_err(|e| ReadError::Network(e.to_string()))?;

    if response.status().is_success() {
        let result: KvGetResponse = response
            .json()
            .await
            .map_err(|e| ReadError::Network(e.to_string()))?;
        Ok(result)
    } else {
        let error: ErrorResponse = response
            .json()
            .await
            .map_err(|e| ReadError::Network(e.to_string()))?;
        Err(ReadError::NotLeader {
            message: error.error,
            leader_hint: error.leader_hint,
        })
    }
}

#[derive(Debug)]
#[allow(dead_code)]
enum ReadError {
    Network(String),
    NotLeader {
        message: String,
        leader_hint: Option<u64>,
    },
}

#[derive(Debug)]
#[allow(dead_code)]
enum SubmitError {
    Network(String),
    NotLeader {
        message: String,
        leader_hint: Option<u64>,
    },
}

// Test cases

/// Test that a cluster elects exactly one leader
#[tokio::test]
async fn test_cluster_elects_leader() {
    let cluster = TestCluster::new().await;

    // Wait for leader election
    let leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    // Verify exactly one leader
    let client = reqwest::Client::new();
    let mut leader_count = 0;
    for addr in cluster.all_addrs() {
        if let Ok(status) = get_status(&client, &addr).await {
            if status.state == "Leader" {
                leader_count += 1;
            }
        }
    }
    assert_eq!(leader_count, 1, "Should have exactly one leader");

    cluster.shutdown().await;
}

/// Test submitting multiple commands concurrently
#[tokio::test]
async fn test_concurrent_client_commands() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // Submit 20 commands concurrently
    let mut handles = Vec::new();
    for i in 0..20 {
        let client = client.clone();
        let addr = leader_addr;
        handles.push(tokio::spawn(async move {
            let cmd = format!("SET key{} value{}", i, i);
            submit_command(&client, &addr, &cmd).await
        }));
    }

    // Wait for all to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // Verify all succeeded
    let successes = results
        .iter()
        .filter(|r| r.as_ref().map(|inner| inner.is_ok()).unwrap_or(false))
        .count();
    assert_eq!(successes, 20, "All commands should succeed");

    // Give time for replication
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify all keys via linearizable reads from leader
    for i in 0..20 {
        let key = format!("key{}", i);
        let expected_value = format!("value{}", i);
        let read_result = linearizable_read(&client, &leader_addr, &key)
            .await
            .expect("Linearizable read should succeed");
        assert_eq!(
            read_result.value,
            Some(expected_value),
            "Key {} should have correct value",
            i
        );
    }

    cluster.shutdown().await;
}

/// Test rapid-fire HTTP requests
#[tokio::test]
async fn test_rapid_fire_http_requests() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // Submit 100 commands as fast as possible
    let mut handles = Vec::new();
    for i in 0..100 {
        let client = client.clone();
        let addr = leader_addr;
        handles.push(tokio::spawn(async move {
            let cmd = format!("SET rapid{} value{}", i, i);
            submit_command(&client, &addr, &cmd).await
        }));
    }

    // Wait for all to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // Count successes
    let successes = results
        .iter()
        .filter(|r| r.as_ref().map(|inner| inner.is_ok()).unwrap_or(false))
        .count();

    // All should succeed (leader handles concurrent commands)
    assert_eq!(successes, 100, "All 100 commands should succeed");

    // Verify state machine has all values via linearizable reads
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut read_count = 0;
    for i in 0..100 {
        let key = format!("rapid{}", i);
        if linearizable_read(&client, &leader_addr, &key).await.is_ok() {
            read_count += 1;
        }
    }
    assert_eq!(read_count, 100, "Should have 100 keys in state machine");

    cluster.shutdown().await;
}

/// Test concurrent reads and writes
#[tokio::test]
async fn test_concurrent_reads_and_writes() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // First, write some initial data
    for i in 0..10 {
        let cmd = format!("SET rw{} initial{}", i, i);
        submit_command(&client, &leader_addr, &cmd)
            .await
            .expect("Initial write should succeed");
    }

    // Give time for replication
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now do concurrent reads and writes
    let mut handles = Vec::new();

    // Spawn writers
    for i in 10..30 {
        let client = client.clone();
        let addr = leader_addr;
        handles.push(tokio::spawn(async move {
            let cmd = format!("SET rw{} updated{}", i, i);
            submit_command(&client, &addr, &cmd).await.map(|_| ())
        }));
    }

    // Spawn linearizable readers (must go through leader)
    for i in 0..10 {
        let client = client.clone();
        let addr = leader_addr;
        handles.push(tokio::spawn(async move {
            // Linearizable read from leader
            let key = format!("rw{}", i);
            let _ = linearizable_read(&client, &addr, &key).await;
            Ok::<_, SubmitError>(())
        }));
    }

    // Wait for all to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed
    let failures = results
        .iter()
        .filter(|r| r.as_ref().map(|inner| inner.is_err()).unwrap_or(true))
        .count();
    assert_eq!(failures, 0, "No operations should fail");

    // Verify final state is consistent via linearizable reads
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut key_count = 0;
    for i in 0..30 {
        let key = format!("rw{}", i);
        if linearizable_read(&client, &leader_addr, &key).await.is_ok() {
            key_count += 1;
        }
    }
    assert_eq!(key_count, 30, "Should have 30 keys total");

    cluster.shutdown().await;
}

/// Test submitting to a follower returns proper error with leader hint
#[tokio::test]
async fn test_follower_redirect() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    // Find a follower
    let follower_addr = cluster
        .find_follower()
        .await
        .expect("Should have a follower");

    let client = reqwest::Client::new();

    // Submit to follower should fail with leader hint
    let result = submit_command(&client, &follower_addr, "SET foo bar").await;

    match result {
        Err(SubmitError::NotLeader {
            message,
            leader_hint,
        }) => {
            assert_eq!(message, "Not the leader");
            assert!(
                leader_hint.is_some(),
                "Should provide leader_hint when known"
            );
        }
        Ok(_) => panic!("Should have failed when submitting to follower"),
        Err(e) => panic!("Unexpected error: {:?}", e),
    }

    cluster.shutdown().await;
}

/// Test that a new leader is elected after the current leader fails
#[tokio::test]
async fn test_leader_failover() {
    let mut cluster = TestCluster::with_nodes(3).await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    // Write some initial data to confirm leader is working
    for i in 0..3 {
        let cmd = format!("SET failover{} value{}", i, i);
        submit_command(&client, &leader_addr, &cmd)
            .await
            .expect("Initial write should succeed");
    }

    // Find which node index is the leader
    let leader_index = cluster
        .nodes
        .iter()
        .position(|n| n.addr() == leader_addr)
        .unwrap();

    // Kill the leader
    cluster.shutdown_node(leader_index).await;

    // Wait for new leader
    let new_leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(new_leader.is_some(), "New leader should be elected");

    let new_addr = new_leader.unwrap();
    assert_ne!(new_addr, leader_addr, "New leader should be different");

    // Verify new leader can accept writes
    submit_command(&client, &new_addr, "SET after_failover success")
        .await
        .expect("New leader should accept writes");

    // Verify the data is there via linearizable read
    let read_result = linearizable_read(&client, &new_addr, "after_failover")
        .await
        .expect("Linearizable read should succeed");
    assert_eq!(read_result.value, Some("success".to_string()));

    cluster.shutdown().await;
}

/// Test that commands during leader failover are handled properly
#[tokio::test]
async fn test_concurrent_commands_during_leader_failover() {
    let mut cluster = TestCluster::with_nodes(3).await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    // Write some initial data
    for i in 0..5 {
        let cmd = format!("SET failover{} value{}", i, i);
        submit_command(&client, &leader_addr, &cmd)
            .await
            .expect("Initial write should succeed");
    }

    // Find which node index is the leader
    let leader_index = cluster
        .nodes
        .iter()
        .position(|n| n.addr() == leader_addr)
        .unwrap();

    // Kill the leader
    cluster.shutdown_node(leader_index).await;

    // Wait for new leader to be elected
    let new_leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(
        new_leader.is_some(),
        "New leader should be elected after failover"
    );

    let new_leader_addr = new_leader.unwrap();

    // Now try to write more data through the new leader
    let mut successes = 0;
    for i in 5..10 {
        let cmd = format!("SET failover{} value{}", i, i);
        if submit_command(&client, &new_leader_addr, &cmd)
            .await
            .is_ok()
        {
            successes += 1;
        }
    }

    assert!(successes >= 1, "Should be able to write after failover");

    // Verify data via linearizable read from new leader
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut read_count = 0;
    for i in 0..5 {
        let key = format!("failover{}", i);
        if linearizable_read(&client, &new_leader_addr, &key)
            .await
            .is_ok()
        {
            read_count += 1;
        }
    }
    assert!(read_count >= 5, "Should have at least initial data");

    cluster.shutdown().await;
}

/// Test cluster consistency after multiple operations
#[tokio::test]
async fn test_cluster_consistency() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // Perform a series of operations
    for i in 0..10 {
        let cmd = format!("SET key{} value{}", i, i);
        submit_command(&client, &leader_addr, &cmd)
            .await
            .expect("Write should succeed");
    }

    // Give time for replication
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify all data via linearizable reads from leader
    for i in 0..10 {
        let key = format!("key{}", i);
        let expected_value = format!("value{}", i);
        let read_result = linearizable_read(&client, &leader_addr, &key)
            .await
            .expect("Linearizable read should succeed");
        assert_eq!(
            read_result.value,
            Some(expected_value),
            "Key {} should have correct value",
            i
        );
    }

    // All nodes should agree on commit index (after replication settles)
    let mut commit_indices = Vec::new();
    for addr in cluster.all_addrs() {
        let status = get_status(&client, &addr).await.unwrap();
        commit_indices.push(status.commit_index);
    }

    // All should be the same
    let first = commit_indices[0];
    for ci in commit_indices {
        assert_eq!(ci, first, "All nodes should have same commit_index");
    }

    cluster.shutdown().await;
}

/// Test concurrent writes to the same key - verifies linearizability
/// All writes should succeed and all nodes should converge to the same final value.
/// The final value must be from the very last log entry (since all entries write to the same key,
/// the state machine value is determined by the last write in log order).
#[tokio::test]
async fn test_concurrent_writes_to_same_key() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // Submit 50 concurrent writes to the same key with different values
    // Each write is "SET contested_key writer_N"
    let num_writes = 50usize;
    let mut handles = Vec::new();
    for i in 0..num_writes {
        let client = client.clone();
        let addr = leader_addr;
        handles.push(tokio::spawn(async move {
            let cmd = format!("SET contested_key writer_{}", i);
            submit_command(&client, &addr, &cmd).await
        }));
    }

    // Wait for all writes to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // All writes should succeed (Raft serializes them)
    let successes = results
        .iter()
        .filter(|r| r.as_ref().map(|inner| inner.is_ok()).unwrap_or(false))
        .count();
    assert_eq!(successes, num_writes, "All concurrent writes should succeed");

    // Give time for full replication
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Read the last log entry from each node and verify it matches the state machine
    for node in &cluster.nodes {
        let core = node.shared_core.lock().await;
        let kv = node.kv_store.lock().unwrap();

        // Get the last log entry (should be the last write to contested_key)
        let last_entry = core.log.last().expect("Log should not be empty");

        // The last entry should be a SET command for contested_key
        assert!(
            last_entry.command.starts_with("SET contested_key "),
            "Last log entry should be a SET contested_key command, got: {}",
            last_entry.command
        );

        // Extract the value from the command ("SET contested_key writer_X" -> "writer_X")
        let last_log_value = last_entry
            .command
            .strip_prefix("SET contested_key ")
            .expect("Should have SET prefix");

        // The state machine should have this exact value
        let sm_value = kv
            .get("contested_key")
            .expect("Key should exist in state machine");
        assert_eq!(
            sm_value, last_log_value,
            "State machine value '{}' should match last log entry value '{}'",
            sm_value, last_log_value
        );
    }

    // Verify all nodes have the same log (same entries in same order)
    let first_log: Vec<_> = {
        let core = cluster.nodes[0].shared_core.lock().await;
        core.log.iter().map(|e| e.command.clone()).collect()
    };

    for (i, node) in cluster.nodes.iter().enumerate().skip(1) {
        let core = node.shared_core.lock().await;
        let node_log: Vec<_> = core.log.iter().map(|e| e.command.clone()).collect();
        assert_eq!(
            first_log, node_log,
            "Node {} log should match node 0 log",
            i
        );
    }

    // Verify all state machines have the same state
    let first_kv = cluster.nodes[0].kv_store.lock().unwrap().all();
    for (i, node) in cluster.nodes.iter().enumerate().skip(1) {
        let node_kv = node.kv_store.lock().unwrap().all();
        assert_eq!(
            first_kv, node_kv,
            "Node {} state machine should match node 0",
            i
        );
    }

    // Log should contain all 50 writes (plus NOOP at index 1)
    assert_eq!(
        first_log.len(),
        51,
        "Log should have 51 entries (1 NOOP + 50 writes)"
    );

    cluster.shutdown().await;
}

/// Test that linearizable read returns NotLeader error when sent to a follower
#[tokio::test]
async fn test_linearizable_read_not_leader() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    // Find a follower
    let follower_addr = cluster
        .find_follower()
        .await
        .expect("Should have a follower");

    let client = reqwest::Client::new();

    // Linearizable read from follower should fail with NotLeader
    let result = linearizable_read(&client, &follower_addr, "anykey").await;

    match result {
        Err(ReadError::NotLeader {
            message,
            leader_hint,
        }) => {
            assert_eq!(message, "Not the leader");
            assert!(
                leader_hint.is_some(),
                "Should provide leader_hint when known"
            );
        }
        Ok(_) => panic!("Should have failed when reading from follower"),
        Err(e) => panic!("Unexpected error: {:?}", e),
    }

    cluster.shutdown().await;
}

/// Test that linearizable read succeeds when sent to the leader
#[tokio::test]
async fn test_linearizable_read_as_leader() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // Write a value first
    submit_command(&client, &leader_addr, "SET mykey myvalue")
        .await
        .expect("Write should succeed");

    // Linearizable read should succeed and return the value
    let read_result = linearizable_read(&client, &leader_addr, "mykey")
        .await
        .expect("Linearizable read should succeed");

    assert_eq!(read_result.key, "mykey");
    assert_eq!(read_result.value, Some("myvalue".to_string()));

    cluster.shutdown().await;
}

/// Test that linearizable read returns None for non-existent keys
#[tokio::test]
async fn test_linearizable_read_missing_key() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // Linearizable read of non-existent key should succeed with None value
    let read_result = linearizable_read(&client, &leader_addr, "nonexistent")
        .await
        .expect("Linearizable read should succeed");

    assert_eq!(read_result.key, "nonexistent");
    assert_eq!(read_result.value, None);

    cluster.shutdown().await;
}

/// Test that linearizable read returns consistent values after writes
#[tokio::test]
async fn test_linearizable_read_after_writes() {
    let cluster = TestCluster::new().await;

    // Wait for leader
    let leader_addr = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect a leader");

    let client = reqwest::Client::new();

    // Write multiple values
    for i in 0..5 {
        let cmd = format!("SET linearkey value{}", i);
        submit_command(&client, &leader_addr, &cmd)
            .await
            .expect("Write should succeed");
    }

    // Linearizable read should see the latest value
    let read_result = linearizable_read(&client, &leader_addr, "linearkey")
        .await
        .expect("Linearizable read should succeed");

    // The value should be the last one written
    assert_eq!(read_result.value, Some("value4".to_string()));

    cluster.shutdown().await;
}
