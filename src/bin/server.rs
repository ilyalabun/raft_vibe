//! Raft server binary
//!
//! Runs a single Raft node with HTTP API for both cluster communication and clients.
//!
//! Usage: raft-server --id <NODE_ID> --port <PORT> --data-dir <DIR> --peers <PEER1,PEER2,...>
//!
//! Example for a 3-node cluster:
//!   Node 1: raft-server --id 1 --port 8001 --data-dir /tmp/raft1 --peers 2=127.0.0.1:8002,3=127.0.0.1:8003
//!   Node 2: raft-server --id 2 --port 8002 --data-dir /tmp/raft2 --peers 1=127.0.0.1:8001,3=127.0.0.1:8003
//!   Node 3: raft-server --id 3 --port 8003 --data-dir /tmp/raft3 --peers 1=127.0.0.1:8001,2=127.0.0.1:8002

use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::Serialize;

use raft_vibe::api::client_http::create_client_router_full;
use raft_vibe::core::raft_core::RaftCore;
use raft_vibe::core::raft_server::RaftServer;
use raft_vibe::state_machine::kv::{KeyValueStore, SharedKvStore};
use raft_vibe::storage::file::FileStorage;
use raft_vibe::transport::http::{create_router, HttpTransport, SharedCore};

fn parse_args() -> (u64, u16, String, HashMap<u64, String>) {
    let args: Vec<String> = env::args().collect();

    let mut id: Option<u64> = None;
    let mut port: Option<u16> = None;
    let mut data_dir: Option<String> = None;
    let mut peers: HashMap<u64, String> = HashMap::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--id" => {
                id = Some(args[i + 1].parse().expect("Invalid node ID"));
                i += 2;
            }
            "--port" => {
                port = Some(args[i + 1].parse().expect("Invalid port"));
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
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                i += 1;
            }
        }
    }

    let id = id.expect("--id is required");
    let port = port.expect("--port is required");
    let data_dir = data_dir.expect("--data-dir is required");

    (id, port, data_dir, peers)
}

#[tokio::main]
async fn main() {
    let (id, port, data_dir, peers) = parse_args();

    println!("[NODE {}] Starting on port {}", id, port);
    println!("[NODE {}] Data directory: {}", id, data_dir);
    println!("[NODE {}] Peers: {:?}", id, peers);

    // Create storage
    let storage = FileStorage::new(&data_dir).expect("Failed to create storage");

    // Create state machine (shared for both Raft and direct reads)
    let kv_store: SharedKvStore = Arc::new(Mutex::new(KeyValueStore::new()));

    // Create transport
    let peer_ids: Vec<u64> = peers.keys().copied().collect();
    let transport = HttpTransport::new(peers, Duration::from_secs(5));

    // Create RaftCore
    let core = RaftCore::new(
        id,
        peer_ids,
        Box::new(storage),
        Box::new(kv_store.clone()),
    );

    // Create RaftServer
    let (server, shared_core) = RaftServer::new(core, transport);

    // Start the server loop (returns handle for client commands)
    let raft_handle = server.start();

    // Create combined router with both Raft RPC and client API
    let app = create_combined_router(shared_core.clone(), raft_handle, shared_core, kv_store);

    // Start HTTP server
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    println!("[NODE {}] Listening on {}", id, addr);
    println!("[NODE {}] Client API:", id);
    println!("[NODE {}]   POST /client/submit      - Submit a command", id);
    println!("[NODE {}]   GET  /client/get/:key    - Get a value", id);
    println!("[NODE {}]   GET  /client/kv          - Dump all key-values", id);
    println!("[NODE {}]   GET  /client/leader      - Get leader info", id);
    println!("[NODE {}]   GET  /client/status      - Get node status", id);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Response for GET request
#[derive(Serialize)]
struct GetResponse {
    key: String,
    value: Option<String>,
}

/// Handle GET /client/get/:key
async fn handle_get(
    State(kv_store): State<SharedKvStore>,
    Path(key): Path<String>,
) -> (StatusCode, Json<GetResponse>) {
    let value = kv_store.lock().unwrap().get(&key);
    let status = if value.is_some() { StatusCode::OK } else { StatusCode::NOT_FOUND };
    (status, Json(GetResponse { key, value }))
}

/// Handle GET /client/kv - dump entire state
async fn handle_dump(
    State(kv_store): State<SharedKvStore>,
) -> Json<HashMap<String, String>> {
    Json(kv_store.lock().unwrap().all())
}

/// Create a combined router with both Raft RPC and client API routes
fn create_combined_router(
    raft_core: SharedCore,
    raft_handle: raft_vibe::core::raft_server::RaftHandle,
    client_core: SharedCore,
    kv_store: SharedKvStore,
) -> Router {
    // Raft RPC routes (/raft/*)
    let raft_router = create_router(raft_core);

    // Client API routes (/client/*)
    let client_router = create_client_router_full(raft_handle, client_core);

    // KV read endpoints
    let kv_router = Router::new()
        .route("/client/get/{key}", get(handle_get))
        .route("/client/kv", get(handle_dump))
        .with_state(kv_store);

    // Merge routers
    raft_router.merge(client_router).merge(kv_router)
}
