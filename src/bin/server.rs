//! Raft server binary
//!
//! Runs a single Raft node with separate ports for cluster transport and client API.
//!
//! Usage: raft-server --id <NODE_ID> --transport-port <PORT> --api-port <PORT> --data-dir <DIR> --peers <PEER1,PEER2,...> [--snapshot-threshold N]
//!
//! Example for a 3-node cluster:
//!   Node 1: raft-server --id 1 --transport-port 8001 --api-port 9001 --data-dir /tmp/raft1 --peers 2=127.0.0.1:8002,3=127.0.0.1:8003
//!   Node 2: raft-server --id 2 --transport-port 8002 --api-port 9002 --data-dir /tmp/raft2 --peers 1=127.0.0.1:8001,3=127.0.0.1:8003
//!   Node 3: raft-server --id 3 --transport-port 8003 --api-port 9003 --data-dir /tmp/raft3 --peers 1=127.0.0.1:8001,2=127.0.0.1:8002
//!
//! Options:
//!   --snapshot-threshold N    Take snapshot after N entries (default: 1000, set to 0 to disable)
//!
//! Ports:
//!   --transport-port: Used for Raft RPC between nodes (/raft/* endpoints)
//!   --api-port: Used for client requests (/client/* endpoints)

use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use raft_vibe::api::client_http::create_client_router_with_reads;
use raft_vibe::core::raft_core::RaftCore;
use raft_vibe::core::raft_server::RaftServer;
use raft_vibe::state_machine::kv::{KeyValueStore, SharedKvStore};
use raft_vibe::storage::file::FileStorage;
use raft_vibe::transport::http::{create_router, HttpTransport};

fn parse_args() -> (u64, u16, u16, String, HashMap<u64, String>, u64) {
    let args: Vec<String> = env::args().collect();

    let mut id: Option<u64> = None;
    let mut transport_port: Option<u16> = None;
    let mut api_port: Option<u16> = None;
    let mut data_dir: Option<String> = None;
    let mut peers: HashMap<u64, String> = HashMap::new();
    let mut snapshot_threshold: u64 = 1000; // Default

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--id" => {
                id = Some(args[i + 1].parse().expect("Invalid node ID"));
                i += 2;
            }
            "--transport-port" => {
                transport_port = Some(args[i + 1].parse().expect("Invalid transport port"));
                i += 2;
            }
            "--api-port" => {
                api_port = Some(args[i + 1].parse().expect("Invalid API port"));
                i += 2;
            }
            "--data-dir" => {
                data_dir = Some(args[i + 1].clone());
                i += 2;
            }
            "--peers" => {
                // Format: 2=127.0.0.1:8002,3=127.0.0.1:8003
                for peer_spec in args[i + 1].split(',') {
                    let parts: Vec<&str> = peer_spec.split('=').collect();
                    if parts.len() == 2 {
                        let peer_id: u64 = parts[0].parse().expect("Invalid peer ID");
                        peers.insert(peer_id, parts[1].to_string());
                    }
                }
                i += 2;
            }
            "--snapshot-threshold" => {
                snapshot_threshold = args[i + 1].parse().expect("Invalid snapshot threshold");
                i += 2;
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                i += 1;
            }
        }
    }

    let id = id.expect("--id is required");
    let transport_port = transport_port.expect("--transport-port is required");
    let api_port = api_port.expect("--api-port is required");
    let data_dir = data_dir.expect("--data-dir is required");

    (id, transport_port, api_port, data_dir, peers, snapshot_threshold)
}

#[tokio::main]
async fn main() {
    let (id, transport_port, api_port, data_dir, peers, snapshot_threshold) = parse_args();

    println!("[NODE {}] Starting with transport port {} and API port {}", id, transport_port, api_port);
    println!("[NODE {}] Data directory: {}", id, data_dir);
    println!("[NODE {}] Peers: {:?}", id, peers);
    println!("[NODE {}] Snapshot threshold: {} entries", id, snapshot_threshold);

    // Create storage
    let storage = FileStorage::new(&data_dir).expect("Failed to create storage");

    // Create state machine (shared for both Raft and direct reads)
    let kv_store: SharedKvStore = Arc::new(Mutex::new(KeyValueStore::new()));

    // Create transport
    let peer_ids: Vec<u64> = peers.keys().copied().collect();
    let transport = HttpTransport::new(peers, Duration::from_secs(5));

    // Create RaftCore
    let mut core = RaftCore::new(
        id,
        peer_ids,
        Box::new(storage),
        Box::new(kv_store.clone()),
    );

    // Configure snapshot threshold
    core.set_snapshot_threshold(snapshot_threshold);

    // Create RaftServer
    let (server, shared_core) = RaftServer::new(core, transport);

    // Start the server loop (returns handle for client commands)
    let raft_handle = server.start();

    // Create separate routers for transport and API
    let raft_router = create_router(shared_core.clone());
    let client_router = create_client_router_with_reads(raft_handle, shared_core, kv_store);

    // Start transport server (for Raft RPC)
    let transport_addr: SocketAddr = format!("0.0.0.0:{}", transport_port).parse().unwrap();
    println!("[NODE {}] Transport server listening on {}", id, transport_addr);
    println!("[NODE {}]   POST /raft/request_vote     - RequestVote RPC", id);
    println!("[NODE {}]   POST /raft/append_entries   - AppendEntries RPC", id);
    println!("[NODE {}]   POST /raft/install_snapshot - InstallSnapshot RPC", id);

    let transport_listener = tokio::net::TcpListener::bind(transport_addr).await.unwrap();
    tokio::spawn(async move {
        axum::serve(transport_listener, raft_router).await.unwrap();
    });

    // Start API server (for client requests)
    let api_addr: SocketAddr = format!("0.0.0.0:{}", api_port).parse().unwrap();
    println!("[NODE {}] API server listening on {}", id, api_addr);
    println!("[NODE {}]   POST /client/submit       - Submit a command", id);
    println!("[NODE {}]   GET  /client/read/:key   - Linearizable read", id);
    println!("[NODE {}]   GET  /client/leader      - Get leader info", id);
    println!("[NODE {}]   GET  /client/status      - Get node status", id);

    let api_listener = tokio::net::TcpListener::bind(api_addr).await.unwrap();
    axum::serve(api_listener, client_router).await.unwrap();
}

