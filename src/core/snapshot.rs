//! Snapshot types for log compaction
//!
//! Snapshots allow discarding old log entries by capturing
//! the state machine state at a specific log index.

use serde::{Deserialize, Serialize};

/// Metadata about what's included in a snapshot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMetadata {
    /// Last log index included in this snapshot
    pub last_included_index: u64,
    /// Term of the last included log entry
    pub last_included_term: u64,
}

/// A snapshot of the state machine at a specific point in the log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub metadata: SnapshotMetadata,
    /// Serialized state machine data
    pub data: Vec<u8>,
}
