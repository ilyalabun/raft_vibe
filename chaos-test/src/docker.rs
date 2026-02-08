//! Docker-based cluster management for chaos testing
//!
//! Provides `DockerCluster` which manages a multi-node Raft cluster running in
//! Docker containers. Supports node crashes, network partitions (via `docker network`
//! and `iptables`), and latency injection (via `tc`/`netem`).

use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;
use tokio::time::sleep;

/// Status response from the Raft node's /client/status endpoint
#[derive(Debug, Deserialize)]
struct StatusResponse {
    node_id: u64,
    state: String,
}

/// Manages a Docker Compose-based Raft cluster for chaos testing
pub struct DockerCluster {
    compose_file: String,
    node_count: usize,
    /// Docker Compose project name (used as network name prefix)
    project_name: String,
}

impl DockerCluster {
    /// Start a new Docker cluster.
    ///
    /// Tears down any existing containers first to ensure clean state,
    /// then starts all containers via `docker compose up -d` (images must be pre-built).
    pub async fn start(compose_file: &str, node_count: usize) -> Self {
        // Detect the project name that docker compose uses
        let project_output = Command::new("docker")
            .args(["compose", "-f", compose_file, "config", "--format", "json"])
            .output()
            .await
            .expect("Failed to run docker compose config");

        let project_name = if project_output.status.success() {
            let config_str = String::from_utf8_lossy(&project_output.stdout);
            // Parse the JSON to get the "name" field
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&config_str) {
                config["name"].as_str().unwrap_or("raft_vibe").to_string()
            } else {
                "raft_vibe".to_string()
            }
        } else {
            "raft_vibe".to_string()
        };

        let cluster = DockerCluster {
            compose_file: compose_file.to_string(),
            node_count,
            project_name,
        };

        // Tear down any existing containers to ensure clean state (no stale data)
        let _ = Command::new("docker")
            .args(["compose", "-f", &cluster.compose_file, "down", "-v"])
            .output()
            .await;

        // Start containers (--build ensures image is up-to-date)
        let output = Command::new("docker")
            .args(["compose", "-f", &cluster.compose_file, "up", "-d", "--build"])
            .output()
            .await
            .expect("Failed to run docker compose up");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("docker compose up failed: {}", stderr);
        }

        println!("Docker cluster started with {} nodes", node_count);

        cluster
    }

    /// Stop and remove all containers and volumes
    pub async fn stop(&self) {
        let output = Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "down", "-v"])
            .output()
            .await
            .expect("Failed to run docker compose down");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("docker compose down warning: {}", stderr);
        }

        println!("Docker cluster stopped");
    }

    /// Get the host-mapped API address for a node
    pub fn node_api_addr(&self, id: usize) -> String {
        format!("127.0.0.1:{}", 9100 + id)
    }

    /// Wait for a leader to be elected within the given timeout
    pub async fn wait_for_leader(&self, timeout: Duration) -> Option<u64> {
        let deadline = tokio::time::Instant::now() + timeout;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("Failed to create HTTP client");

        while tokio::time::Instant::now() < deadline {
            for id in 1..=self.node_count {
                let url = format!("http://{}/client/status", self.node_api_addr(id));
                if let Ok(resp) = client.get(&url).send().await {
                    if let Ok(status) = resp.json::<StatusResponse>().await {
                        if status.state == "Leader" {
                            println!("Leader elected: node {} (term detected via status)", status.node_id);
                            return Some(status.node_id);
                        }
                    }
                }
            }
            sleep(Duration::from_millis(500)).await;
        }

        None
    }

    /// Find the current leader by polling /client/status on all nodes
    pub async fn find_leader(&self) -> Option<u64> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("Failed to create HTTP client");

        for id in 1..=self.node_count {
            let url = format!("http://{}/client/status", self.node_api_addr(id));
            if let Ok(resp) = client.get(&url).send().await {
                if let Ok(status) = resp.json::<StatusResponse>().await {
                    if status.state == "Leader" {
                        return Some(status.node_id);
                    }
                }
            }
        }
        None
    }

    // --- Node crashes ---

    /// Kill a node immediately (SIGKILL)
    pub async fn kill_node(&self, id: usize) {
        let container = format!("raft-node-{}", id);
        let output = Command::new("docker")
            .args(["kill", &container])
            .output()
            .await
            .expect("Failed to run docker kill");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("docker kill {} warning: {}", container, stderr);
        } else {
            println!("Killed node {}", id);
        }
    }

    /// Start a previously stopped/killed node
    pub async fn start_node(&self, id: usize) {
        let container = format!("raft-node-{}", id);
        let output = Command::new("docker")
            .args(["start", &container])
            .output()
            .await
            .expect("Failed to run docker start");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("docker start {} warning: {}", container, stderr);
        } else {
            println!("Started node {}", id);
        }
    }

    /// Kill the current leader
    pub async fn kill_leader(&self) -> Option<u64> {
        if let Some(leader_id) = self.find_leader().await {
            self.kill_node(leader_id as usize).await;
            println!("Killed leader node {}", leader_id);
            Some(leader_id)
        } else {
            eprintln!("No leader found to kill");
            None
        }
    }

    /// Get the full Docker network name (project_name + network name from compose)
    fn network_name(&self) -> String {
        format!("{}_raft-chaos-net", self.project_name)
    }

    // --- Network partitions (docker network) ---

    /// Isolate a node by disconnecting it from the cluster network
    pub async fn isolate_node(&self, id: usize) {
        let container = format!("raft-node-{}", id);
        let network = self.network_name();
        let output = Command::new("docker")
            .args(["network", "disconnect", &network, &container])
            .output()
            .await
            .expect("Failed to run docker network disconnect");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("docker network disconnect {} warning: {}", container, stderr);
        } else {
            println!("Isolated node {} from network", id);
        }
    }

    /// Rejoin a previously isolated node to the cluster network
    pub async fn rejoin_node(&self, id: usize) {
        let container = format!("raft-node-{}", id);
        let network = self.network_name();
        let output = Command::new("docker")
            .args(["network", "connect", &network, &container])
            .output()
            .await
            .expect("Failed to run docker network connect");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("docker network connect {} warning: {}", container, stderr);
        } else {
            println!("Rejoined node {} to network", id);
        }
    }

    // --- Granular partitions (iptables inside containers) ---

    /// Get a container's IP address on the raft-chaos-net network
    pub async fn get_container_ip(&self, id: usize) -> Option<String> {
        let container = format!("raft-node-{}", id);
        let output = Command::new("docker")
            .args([
                "inspect",
                "-f",
                "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
                &container,
            ])
            .output()
            .await
            .expect("Failed to run docker inspect");

        if output.status.success() {
            let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if ip.is_empty() {
                None
            } else {
                Some(ip)
            }
        } else {
            None
        }
    }

    /// Create a one-way network partition: `from` cannot communicate with `to`,
    /// but `to` can still send to `from`.
    pub async fn partition_one_way(&self, from: usize, to: usize) {
        let ip_to = self.get_container_ip(to).await
            .unwrap_or_else(|| panic!("Could not get IP for node {}", to));

        // Block traffic only on the `from` node
        self.iptables_rule(from, "-A", &ip_to).await;

        println!("Partitioned node {} -x-> node {} (one-way)", from, to);
    }

    /// Heal a one-way network partition from `from` to `to`
    pub async fn heal_partition_one_way(&self, from: usize, to: usize) {
        let ip_to = self.get_container_ip(to).await
            .unwrap_or_else(|| panic!("Could not get IP for node {}", to));

        // Remove block only on the `from` node
        self.iptables_rule(from, "-D", &ip_to).await;

        println!("Healed one-way partition node {} -x-> node {}", from, to);
    }

    /// Create a symmetric network partition between two nodes using iptables
    pub async fn partition(&self, node_a: usize, node_b: usize) {
        let ip_a = self.get_container_ip(node_a).await
            .unwrap_or_else(|| panic!("Could not get IP for node {}", node_a));
        let ip_b = self.get_container_ip(node_b).await
            .unwrap_or_else(|| panic!("Could not get IP for node {}", node_b));

        // Block traffic on node_a from/to node_b
        self.iptables_rule(node_a, "-A", &ip_b).await;
        // Block traffic on node_b from/to node_a
        self.iptables_rule(node_b, "-A", &ip_a).await;

        println!("Partitioned node {} <-> node {}", node_a, node_b);
    }

    /// Heal a symmetric network partition between two nodes
    pub async fn heal_partition(&self, node_a: usize, node_b: usize) {
        let ip_a = self.get_container_ip(node_a).await
            .unwrap_or_else(|| panic!("Could not get IP for node {}", node_a));
        let ip_b = self.get_container_ip(node_b).await
            .unwrap_or_else(|| panic!("Could not get IP for node {}", node_b));

        // Remove block on node_a from/to node_b
        self.iptables_rule(node_a, "-D", &ip_b).await;
        // Remove block on node_b from/to node_a
        self.iptables_rule(node_b, "-D", &ip_a).await;

        println!("Healed partition node {} <-> node {}", node_a, node_b);
    }

    /// Add or remove iptables INPUT+OUTPUT rules for a given peer IP
    async fn iptables_rule(&self, node_id: usize, action: &str, peer_ip: &str) {
        let container = format!("raft-node-{}", node_id);

        // Block incoming from peer
        let output = Command::new("docker")
            .args([
                "exec", &container,
                "iptables", action, "INPUT", "-s", peer_ip, "-j", "DROP",
            ])
            .output()
            .await
            .expect("Failed to run iptables INPUT rule");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("iptables INPUT {} on {} warning: {}", action, container, stderr);
        }

        // Block outgoing to peer
        let output = Command::new("docker")
            .args([
                "exec", &container,
                "iptables", action, "OUTPUT", "-d", peer_ip, "-j", "DROP",
            ])
            .output()
            .await
            .expect("Failed to run iptables OUTPUT rule");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("iptables OUTPUT {} on {} warning: {}", action, container, stderr);
        }
    }

    // --- Slow network (tc/netem) ---

    /// Add latency to a node's network interface
    pub async fn add_latency(&self, id: usize, delay_ms: u64) {
        let container = format!("raft-node-{}", id);
        let delay = format!("{}ms", delay_ms);
        let output = Command::new("docker")
            .args([
                "exec", &container,
                "tc", "qdisc", "add", "dev", "eth0", "root", "netem", "delay", &delay,
            ])
            .output()
            .await
            .expect("Failed to run tc add delay");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("tc add delay on {} warning: {}", container, stderr);
        } else {
            println!("Added {}ms latency to node {}", delay_ms, id);
        }
    }

    /// Add packet loss to a node's network interface
    pub async fn add_packet_loss(&self, id: usize, percent: u32) {
        let container = format!("raft-node-{}", id);
        let loss = format!("{}%", percent);
        let output = Command::new("docker")
            .args([
                "exec", &container,
                "tc", "qdisc", "add", "dev", "eth0", "root", "netem", "loss", &loss,
            ])
            .output()
            .await
            .expect("Failed to run tc add loss");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("tc add loss on {} warning: {}", container, stderr);
        } else {
            println!("Added {}% packet loss to node {}", percent, id);
        }
    }

    /// Remove all netem rules from a node's network interface
    pub async fn clear_netem(&self, id: usize) {
        let container = format!("raft-node-{}", id);
        let output = Command::new("docker")
            .args([
                "exec", &container,
                "tc", "qdisc", "del", "dev", "eth0", "root",
            ])
            .output()
            .await
            .expect("Failed to run tc del");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("tc del on {} warning: {}", container, stderr);
        } else {
            println!("Cleared netem rules on node {}", id);
        }
    }
}

impl Drop for DockerCluster {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup using std::process::Command
        let _ = std::process::Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "down", "-v"])
            .output();
    }
}
