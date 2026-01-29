//! Transport layer for Raft RPC communication
//!
//! - `InMemoryTransport`: Channel-based transport for testing
//! - `HttpTransport`: HTTP/JSON transport for production

pub mod http;
pub mod inmemory;
pub mod traits;

pub use traits::{Transport, TransportError};
