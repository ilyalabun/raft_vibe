//! State machine abstraction for Raft
//!
//! The state machine is the application logic that Raft coordinates.
//! When log entries are committed, they are applied to the state machine.

use std::sync::{Arc, Mutex};

/// Result of applying a command to the state machine
/// Ok(output) for successful commands, Err(message) for errors
pub type ApplyResult = Result<String, String>;

/// State machine trait - the application logic that Raft coordinates
///
/// Implementations must be deterministic: applying the same commands
/// in the same order must produce the same state on all nodes.
pub trait StateMachine: Send {
    /// Apply a command to the state machine
    /// Returns Ok(output) on success, Err(message) on error
    fn apply(&mut self, command: &str) -> ApplyResult;
}

/// Snapshotable state machine - supports log compaction
///
/// Allows serializing state to bytes and restoring from bytes.
/// Used for creating snapshots to discard old log entries.
pub trait Snapshotable: StateMachine {
    /// Create a snapshot of the current state
    /// Returns serialized state as bytes
    fn snapshot(&self) -> Result<Vec<u8>, String>;

    /// Restore state from a snapshot
    /// Replaces current state with the deserialized snapshot data
    fn restore(&mut self, data: &[u8]) -> Result<(), String>;
}

/// Shared record of applied commands for testing
pub type AppliedCommands = Arc<Mutex<Vec<String>>>;

/// Test state machine that records all applied commands to a shared vec
pub struct TestStateMachine {
    applied: AppliedCommands,
}

impl TestStateMachine {
    pub fn new() -> Self {
        TestStateMachine {
            applied: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create with a shared vec to inspect applied commands from outside
    pub fn new_shared(applied: AppliedCommands) -> Self {
        TestStateMachine { applied }
    }
}

impl StateMachine for TestStateMachine {
    fn apply(&mut self, command: &str) -> ApplyResult {
        self.applied.lock().unwrap().push(command.to_string());
        Ok(String::new())
    }
}

impl Snapshotable for TestStateMachine {
    fn snapshot(&self) -> Result<Vec<u8>, String> {
        let applied = self.applied.lock().unwrap().clone();
        serde_json::to_vec(&applied)
            .map_err(|e| format!("test state machine snapshot failed: {}", e))
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), String> {
        let applied: Vec<String> = serde_json::from_slice(data)
            .map_err(|e| format!("test state machine restore failed: {}", e))?;
        *self.applied.lock().unwrap() = applied;
        Ok(())
    }
}
