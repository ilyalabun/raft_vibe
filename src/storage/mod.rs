//! Storage abstraction for Raft persistent state
//!
//! Raft requires certain state to be persisted to stable storage before
//! responding to RPCs: current_term, voted_for, and log entries.
//!
//! This module provides a trait abstraction allowing different implementations:
//! - `MemoryStorage`: Fast, in-memory storage for testing
//! - `FileStorage`: File-based storage with CRC32 checksums

pub mod file;
pub mod memory;

use crate::core::raft_core::LogEntry;
use std::fmt;

/// Errors that can occur during storage operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// I/O error (e.g., disk full, permission denied)
    Io(String),
    /// Data corruption detected
    Corruption(String),
    /// Storage not initialized
    NotInitialized,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io(msg) => write!(f, "I/O error: {}", msg),
            StorageError::Corruption(msg) => write!(f, "Data corruption: {}", msg),
            StorageError::NotInitialized => write!(f, "Storage not initialized"),
        }
    }
}

impl std::error::Error for StorageError {}

/// Storage trait for Raft persistent state
///
/// Implementations must ensure durability - data must survive crashes.
/// All operations are synchronous to simplify the RaftCore logic.
/// The `Send` bound is required for use in async contexts.
pub trait Storage: Send {
    /// Load the current term from storage
    /// Returns 0 if no term has been persisted (fresh start)
    fn load_term(&self) -> Result<u64, StorageError>;

    /// Save the current term to storage
    /// Must be durable before returning
    fn save_term(&mut self, term: u64) -> Result<(), StorageError>;

    /// Load the voted_for value from storage
    /// Returns None if no vote has been cast in the current term
    fn load_voted_for(&self) -> Result<Option<u64>, StorageError>;

    /// Save the voted_for value to storage
    /// Must be durable before returning
    fn save_voted_for(&mut self, voted_for: Option<u64>) -> Result<(), StorageError>;

    /// Load all log entries from storage
    fn load_log(&self) -> Result<Vec<LogEntry>, StorageError>;

    /// Append entries to the log
    /// Entries are appended after any existing entries
    fn append_log_entries(&mut self, entries: &[LogEntry]) -> Result<(), StorageError>;

    /// Truncate the log from the given index (inclusive)
    /// Removes all entries with index >= from_index
    fn truncate_log(&mut self, from_index: u64) -> Result<(), StorageError>;
}
