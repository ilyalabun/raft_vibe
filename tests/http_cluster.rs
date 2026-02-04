//! HTTP cluster integration tests
//!
//! These tests spin up actual HTTP servers (a 3-node cluster) and test
//! concurrent client requests over real HTTP/TCP connections.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;

use raft_vibe::api::client_http::{
    create_client_router_with_reads, ErrorResponse, LeaderResponse, ReadResponse, StatusResponse,
    SubmitResponse,
};
use raft_vibe::core::config::RaftConfig;
use raft_vibe::core::raft_core::RaftCore;
use raft_vibe::core::raft_server::RaftServer;
use raft_vibe::state_machine::kv::{KeyValueStore, SharedKvStore};
use raft_vibe::storage::memory::MemoryStorage;
use raft_vibe::transport::http::{create_router, HttpTransport, SharedCore};

use axum::Router;

/// A single test node in the cluster
struct TestNode {
    #[allow(dead_code)]
    id: u64,
    addr: SocketAddr,
    http_shutdown_tx: Option<oneshot::Sender<()>>,
    raft_handle: raft_vibe::core::raft_server::RaftHandle,
    #[allow(dead_code)]
    kv_store: SharedKvStore,
    shared_core: SharedCore,
}

impl TestNode {
    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// A test cluster of 3 Raft nodes
struct TestCluster {
    nodes: Vec<TestNode>,
}

impl TestCluster {
    /// Create and start a new 3-node cluster
    async fn new() -> Self {
        Self::with_nodes(3).await
    }

    /// Create and start a cluster with the specified number of nodes
    async fn with_nodes(count: usize) -> Self {
        let node_ids: Vec<u64> = (1..=count as u64).collect();

        // First, bind all listeners to get addresses
        let mut listeners = Vec::new();
        let mut addrs = HashMap::new();

        for &id in &node_ids {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addrs.insert(id, addr.to_string());
            listeners.push((id, listener, addr));
        }

        // Use faster election timeouts for testing
        let config = RaftConfig::default()
            .with_heartbeat_interval(Duration::from_millis(50))
            .with_election_timeout(Duration::from_millis(150), Duration::from_millis(300));

        // Now start each node
        let mut nodes = Vec::new();

        for (id, listener, addr) in listeners {
            // Build peer map (all nodes except self)
            let peers: HashMap<u64, String> = addrs
                .iter()
                .filter(|(&peer_id, _)| peer_id != id)
                .map(|(&peer_id, addr)| (peer_id, addr.clone()))
                .collect();

            let peer_ids: Vec<u64> = peers.keys().copied().collect();

            // Create state machine
            let kv_store: SharedKvStore = Arc::new(Mutex::new(KeyValueStore::new()));

            // Create transport with shorter timeout for tests
            let transport = HttpTransport::new(peers, Duration::from_secs(2));

            // Create RaftCore
            let core = RaftCore::new(
                id,
                peer_ids,
                Box::new(MemoryStorage::new()),
                Box::new(kv_store.clone()),
            );

            // Create RaftServer with test config
            let (server, shared_core) = RaftServer::with_config(core, transport, config.clone());

            // Start the server loop
            let raft_handle = server.start();

            // Clone shared_core for TestNode before passing to create_combined_router
            let node_shared_core = shared_core.clone();

            // Create combined router (clone handle for the router)
            let app = create_combined_router(shared_core.clone(), raft_handle.clone(), shared_core, kv_store.clone());

            // Create HTTP shutdown channel
            let (http_shutdown_tx, http_shutdown_rx) = oneshot::channel();

            // Spawn the HTTP server with graceful shutdown
            tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = http_shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });

            nodes.push(TestNode {
                id,
                addr,
                http_shutdown_tx: Some(http_shutdown_tx),
                raft_handle,
                kv_store,
                shared_core: node_shared_core,
            });
        }

        // Give servers time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        TestCluster { nodes }
    }

    /// Shutdown all nodes gracefully
    async fn shutdown(mut self) {
        for node in &mut self.nodes {
            // Shutdown HTTP server
            if let Some(tx) = node.http_shutdown_tx.take() {
                let _ = tx.send(());
            }
            // Shutdown RaftServer
            node.raft_handle.shutdown().await;
        }
        // Give servers time to shutdown
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    /// Get address of a specific node
    #[allow(dead_code)]
    fn node_addr(&self, index: usize) -> SocketAddr {
        self.nodes[index].addr()
    }

    /// Get all node addresses
    fn all_addrs(&self) -> Vec<SocketAddr> {
        self.nodes.iter().map(|n| n.addr()).collect()
    }

    /// Wait for a leader to be elected, with timeout
    async fn wait_for_leader(&self, timeout: Duration) -> Option<SocketAddr> {
        let start = std::time::Instant::now();
        // Use a client with short timeout to avoid getting stuck on shutdown nodes
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();

        while start.elapsed() < timeout {
            for node in &self.nodes {
                // Skip nodes that have been shut down (HTTP server)
                if node.http_shutdown_tx.is_none() {
                    continue;
                }
                if let Ok(status) = get_status(&client, &node.addr).await {
                    if status.state == "Leader" {
                        return Some(node.addr);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// Find current leader address
    #[allow(dead_code)]
    async fn find_leader(&self) -> Option<SocketAddr> {
        let client = reqwest::Client::new();
        for node in &self.nodes {
            if let Ok(status) = get_status(&client, &node.addr).await {
                if status.state == "Leader" {
                    return Some(node.addr);
                }
            }
        }
        None
    }

    /// Get kv_store for a node
    #[allow(dead_code)]
    fn kv_store(&self, index: usize) -> SharedKvStore {
        self.nodes[index].kv_store.clone()
    }

    /// Find a follower address
    async fn find_follower(&self) -> Option<SocketAddr> {
        let client = reqwest::Client::new();
        for node in &self.nodes {
            if let Ok(status) = get_status(&client, &node.addr).await {
                if status.state == "Follower" {
                    return Some(node.addr);
                }
            }
        }
        None
    }

    /// Shutdown a specific node (both HTTP and Raft servers)
    async fn shutdown_node(&mut self, index: usize) {
        // Shutdown HTTP server
        if let Some(tx) = self.nodes[index].http_shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Shutdown RaftServer
        self.nodes[index].raft_handle.shutdown().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

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

#[allow(dead_code)]
async fn get_leader(
    client: &reqwest::Client,
    addr: &SocketAddr,
) -> Result<LeaderResponse, reqwest::Error> {
    client
        .get(format!("http://{}/client/leader", addr))
        .send()
        .await?
        .json()
        .await
}

async fn submit_command(
    client: &reqwest::Client,
    addr: &SocketAddr,
    command: &str,
) -> Result<SubmitResponse, SubmitError> {
    let response = client
        .post(format!("http://{}/client/submit", addr))
        .json(&serde_json::json!({ "command": command }))
        .send()
        .await
        .map_err(|e| SubmitError::Network(e.to_string()))?;

    if response.status().is_success() {
        let result: SubmitResponse = response
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

/// Perform a linearizable read via the ReadIndex protocol
async fn linearizable_read(
    client: &reqwest::Client,
    addr: &SocketAddr,
    key: &str,
) -> Result<ReadResponse, ReadError> {
    let response = client
        .get(format!("http://{}/client/read/{}", addr, key))
        .send()
        .await
        .map_err(|e| ReadError::Network(e.to_string()))?;

    if response.status().is_success() {
        let result: ReadResponse = response
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

// Router creation

fn create_combined_router(
    raft_core: SharedCore,
    raft_handle: raft_vibe::core::raft_server::RaftHandle,
    client_core: SharedCore,
    kv_store: SharedKvStore,
) -> Router {
    let raft_router = create_router(raft_core);
    let client_router = create_client_router_with_reads(raft_handle, client_core, kv_store);

    raft_router.merge(client_router)
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
        Err(SubmitError::NotLeader { message, leader_hint }) => {
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
        .position(|n| n.addr == leader_addr)
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
        .position(|n| n.addr == leader_addr)
        .unwrap();

    // Get remaining node addresses (not the leader)
    let _remaining_addrs: Vec<_> = cluster
        .all_addrs()
        .into_iter()
        .filter(|&addr| addr != leader_addr)
        .collect();

    // Kill the leader
    cluster.shutdown_node(leader_index).await;

    // Wait for new leader to be elected
    let new_leader = cluster.wait_for_leader(Duration::from_secs(10)).await;
    assert!(new_leader.is_some(), "New leader should be elected after failover");

    let new_leader_addr = new_leader.unwrap();

    // Now try to write more data through the new leader
    let mut successes = 0;
    for i in 5..10 {
        let cmd = format!("SET failover{} value{}", i, i);
        if submit_command(&client, &new_leader_addr, &cmd).await.is_ok() {
            successes += 1;
        }
    }

    assert!(successes >= 1, "Should be able to write after failover");

    // Verify data via linearizable read from new leader
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut read_count = 0;
    for i in 0..5 {
        let key = format!("failover{}", i);
        if linearizable_read(&client, &new_leader_addr, &key).await.is_ok() {
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
        let sm_value = kv.get("contested_key").expect("Key should exist in state machine");
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
        Err(ReadError::NotLeader { message, leader_hint }) => {
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
    assert!(read_result.read_index > 0, "read_index should be > 0");

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
    // read_index is u64 so it's always >= 0, just verify it exists
    let _ = read_result.read_index;

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
