//! Testing utilities for Raft cluster integration tests
//!
//! Provides `TestCluster` for spinning up in-process Raft clusters for testing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;

use axum::Router;

use crate::api::client_http::create_client_router_with_reads;
use crate::core::config::RaftConfig;
use crate::core::raft_core::RaftCore;
use crate::core::raft_server::{RaftHandle, RaftServer};
use crate::state_machine::kv::{KeyValueStore, SharedKvStore};
use crate::storage::memory::MemoryStorage;
use crate::transport::http::{create_router, HttpTransport, SharedCore};

/// A single test node in the cluster
pub struct TestNode {
    /// Node ID
    pub id: u64,
    /// HTTP address
    pub addr: SocketAddr,
    /// HTTP server shutdown channel
    http_shutdown_tx: Option<oneshot::Sender<()>>,
    /// Raft server handle
    pub raft_handle: RaftHandle,
    /// Shared KV store
    pub kv_store: SharedKvStore,
    /// Shared Raft core
    pub shared_core: SharedCore,
}

impl TestNode {
    /// Get the node's address
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// A test cluster of Raft nodes
pub struct TestCluster {
    /// All nodes in the cluster
    pub nodes: Vec<TestNode>,
}

impl TestCluster {
    /// Create and start a new 3-node cluster
    pub async fn new() -> Self {
        Self::with_nodes(3).await
    }

    /// Create and start a cluster with the specified number of nodes
    pub async fn with_nodes(count: usize) -> Self {
        Self::with_nodes_and_config(count, None).await
    }

    /// Create and start a cluster with the specified number of nodes and config
    pub async fn with_nodes_and_config(count: usize, config: Option<RaftConfig>) -> Self {
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
        let config = config.unwrap_or_else(|| {
            RaftConfig::default()
                .with_heartbeat_interval(Duration::from_millis(50))
                .with_election_timeout(Duration::from_millis(150), Duration::from_millis(300))
        });

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
            let app = create_combined_router(
                shared_core.clone(),
                raft_handle.clone(),
                shared_core,
                kv_store.clone(),
            );

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
    pub async fn shutdown(mut self) {
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
    pub fn node_addr(&self, index: usize) -> SocketAddr {
        self.nodes[index].addr()
    }

    /// Get all node addresses
    pub fn all_addrs(&self) -> Vec<SocketAddr> {
        self.nodes.iter().map(|n| n.addr()).collect()
    }

    /// Get all node addresses as strings (for chaos-test client)
    pub fn all_addr_strings(&self) -> Vec<String> {
        self.nodes.iter().map(|n| n.addr().to_string()).collect()
    }

    /// Wait for a leader to be elected, with timeout
    pub async fn wait_for_leader(&self, timeout: Duration) -> Option<SocketAddr> {
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
                if let Ok(response) = client
                    .get(format!("http://{}/client/status", node.addr))
                    .send()
                    .await
                {
                    if let Ok(status) = response.json::<StatusResponse>().await {
                        if status.state == "Leader" {
                            return Some(node.addr);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// Find current leader address
    pub async fn find_leader(&self) -> Option<SocketAddr> {
        let client = reqwest::Client::new();
        for node in &self.nodes {
            if let Ok(response) = client
                .get(format!("http://{}/client/status", node.addr))
                .send()
                .await
            {
                if let Ok(status) = response.json::<StatusResponse>().await {
                    if status.state == "Leader" {
                        return Some(node.addr);
                    }
                }
            }
        }
        None
    }

    /// Get kv_store for a node
    pub fn kv_store(&self, index: usize) -> SharedKvStore {
        self.nodes[index].kv_store.clone()
    }

    /// Find a follower address
    pub async fn find_follower(&self) -> Option<SocketAddr> {
        let client = reqwest::Client::new();
        for node in &self.nodes {
            if let Ok(response) = client
                .get(format!("http://{}/client/status", node.addr))
                .send()
                .await
            {
                if let Ok(status) = response.json::<StatusResponse>().await {
                    if status.state == "Follower" {
                        return Some(node.addr);
                    }
                }
            }
        }
        None
    }

    /// Shutdown a specific node (both HTTP and Raft servers)
    pub async fn shutdown_node(&mut self, index: usize) {
        // Shutdown HTTP server
        if let Some(tx) = self.nodes[index].http_shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Shutdown RaftServer
        self.nodes[index].raft_handle.shutdown().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Response for status query (duplicated here to avoid circular dependency)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StatusResponse {
    node_id: u64,
    state: String,
    term: u64,
    leader_id: Option<u64>,
    commit_index: u64,
    last_applied: u64,
    log_length: u64,
}

/// Create combined router for raft transport and client API
fn create_combined_router(
    raft_core: SharedCore,
    raft_handle: RaftHandle,
    client_core: SharedCore,
    kv_store: SharedKvStore,
) -> Router {
    let raft_router = create_router(raft_core);
    let client_router = create_client_router_with_reads(raft_handle, client_core, kv_store);

    raft_router.merge(client_router)
}
