//! Storage layer for Raft persistent state
//!
//! - `MemoryStorage`: Fast, in-memory storage for testing
//! - `FileStorage`: File-based storage with CRC32 checksums

pub mod file;
pub mod memory;
pub mod traits;

pub use traits::{Storage, StorageError};
