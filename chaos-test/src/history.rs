//! History types for recording concurrent operations
//!
//! Provides types for tracking the timing and results of concurrent operations
//! during linearizability testing.

use std::time::Instant;

/// Microsecond timestamp relative to test start
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// Create a timestamp from microseconds
    pub fn from_micros(micros: u64) -> Self {
        Timestamp(micros)
    }

    /// Get the value in microseconds
    pub fn as_micros(&self) -> u64 {
        self.0
    }
}

/// Client/process identifier
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientId(pub u64);

impl ClientId {
    /// Create a new client ID
    pub fn new(id: u64) -> Self {
        ClientId(id)
    }
}

/// Operation type for single-key register
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// Read operation
    Read,
    /// Write operation with value
    Write { value: String },
}

/// Result of an operation
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpResult {
    /// Read returned this value (None = key not found)
    ReadOk(Option<String>),
    /// Write acknowledged
    WriteOk,
    /// Operation failed with error message
    Error(String),
}

/// A completed operation with timing interval
#[derive(Clone, Debug)]
pub struct Operation {
    /// Unique operation ID
    pub id: u64,
    /// Client that performed the operation
    pub client_id: ClientId,
    /// Key this operation targets
    pub key: String,
    /// Type of operation (read or write)
    pub kind: OpKind,
    /// When operation started (invocation time)
    pub invoke_ts: Timestamp,
    /// When operation completed (response time)
    pub complete_ts: Timestamp,
    /// What was returned
    pub result: OpResult,
}

impl Operation {
    /// Create a new operation
    pub fn new(
        id: u64,
        client_id: ClientId,
        key: String,
        kind: OpKind,
        invoke_ts: Timestamp,
        complete_ts: Timestamp,
        result: OpResult,
    ) -> Self {
        Operation {
            id,
            client_id,
            key,
            kind,
            invoke_ts,
            complete_ts,
            result,
        }
    }

    /// Check if this operation is a read
    pub fn is_read(&self) -> bool {
        matches!(self.kind, OpKind::Read)
    }

    /// Check if this operation is a write
    pub fn is_write(&self) -> bool {
        matches!(self.kind, OpKind::Write { .. })
    }

    /// Check if this operation overlaps with another in time
    pub fn overlaps(&self, other: &Operation) -> bool {
        // Two intervals [a, b] and [c, d] overlap if a <= d and c <= b
        self.invoke_ts.0 <= other.complete_ts.0 && other.invoke_ts.0 <= self.complete_ts.0
    }

    /// Check if this operation could linearize at a given timestamp
    /// (invoke <= ts <= complete)
    pub fn could_linearize_at(&self, ts: Timestamp) -> bool {
        self.invoke_ts.0 <= ts.0 && ts.0 <= self.complete_ts.0
    }
}

/// Collection of operations from a test run
#[derive(Clone, Debug)]
pub struct History {
    /// All completed operations
    ops: Vec<Operation>,
    /// Test start time for relative timestamps
    start_time: Instant,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// Create a new empty history
    pub fn new() -> Self {
        History {
            ops: Vec::new(),
            start_time: Instant::now(),
        }
    }

    /// Get current time relative to test start as a Timestamp
    pub fn now(&self) -> Timestamp {
        let elapsed = self.start_time.elapsed();
        Timestamp(elapsed.as_micros() as u64)
    }

    /// Add a completed operation to the history
    pub fn add(&mut self, op: Operation) {
        self.ops.push(op);
    }

    /// Get all operations
    pub fn ops(&self) -> &[Operation] {
        &self.ops
    }

    /// Get number of operations
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Check if history is empty
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Get operations sorted by invoke time
    pub fn sorted_by_invoke_time(&self) -> Vec<&Operation> {
        let mut sorted: Vec<_> = self.ops.iter().collect();
        sorted.sort_by_key(|op| op.invoke_ts);
        sorted
    }

    /// Get only successful operations (no errors)
    pub fn successful_ops(&self) -> Vec<&Operation> {
        self.ops
            .iter()
            .filter(|op| !matches!(op.result, OpResult::Error(_)))
            .collect()
    }

    /// Get operations for a specific key
    pub fn ops_for_key(&self, key: &str) -> Vec<&Operation> {
        self.ops.iter().filter(|op| op.key == key).collect()
    }

    /// Get unique keys from all operations
    pub fn unique_keys(&self) -> std::collections::HashSet<&str> {
        self.ops.iter().map(|op| op.key.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_ordering() {
        let t1 = Timestamp(100);
        let t2 = Timestamp(200);
        assert!(t1 < t2);
        assert_eq!(t1, Timestamp(100));
    }

    #[test]
    fn test_operation_overlap() {
        // Op1: [100, 300]
        let op1 = Operation::new(
            1,
            ClientId(1),
            "x".to_string(),
            OpKind::Write {
                value: "a".to_string(),
            },
            Timestamp(100),
            Timestamp(300),
            OpResult::WriteOk,
        );

        // Op2: [200, 400] - overlaps with op1
        let op2 = Operation::new(
            2,
            ClientId(2),
            "x".to_string(),
            OpKind::Read,
            Timestamp(200),
            Timestamp(400),
            OpResult::ReadOk(Some("a".to_string())),
        );

        // Op3: [500, 600] - does not overlap with op1
        let op3 = Operation::new(
            3,
            ClientId(3),
            "x".to_string(),
            OpKind::Read,
            Timestamp(500),
            Timestamp(600),
            OpResult::ReadOk(Some("a".to_string())),
        );

        assert!(op1.overlaps(&op2));
        assert!(op2.overlaps(&op1));
        assert!(!op1.overlaps(&op3));
        assert!(!op3.overlaps(&op1));
    }

    #[test]
    fn test_could_linearize_at() {
        let op = Operation::new(
            1,
            ClientId(1),
            "x".to_string(),
            OpKind::Write {
                value: "a".to_string(),
            },
            Timestamp(100),
            Timestamp(300),
            OpResult::WriteOk,
        );

        assert!(op.could_linearize_at(Timestamp(100))); // at invoke
        assert!(op.could_linearize_at(Timestamp(200))); // in middle
        assert!(op.could_linearize_at(Timestamp(300))); // at complete
        assert!(!op.could_linearize_at(Timestamp(99))); // before invoke
        assert!(!op.could_linearize_at(Timestamp(301))); // after complete
    }

    #[test]
    fn test_history_add_and_retrieve() {
        let mut history = History::new();

        let op1 = Operation::new(
            1,
            ClientId(1),
            "x".to_string(),
            OpKind::Write {
                value: "a".to_string(),
            },
            Timestamp(100),
            Timestamp(200),
            OpResult::WriteOk,
        );

        let op2 = Operation::new(
            2,
            ClientId(1),
            "x".to_string(),
            OpKind::Read,
            Timestamp(300),
            Timestamp(400),
            OpResult::ReadOk(Some("a".to_string())),
        );

        history.add(op1);
        history.add(op2);

        assert_eq!(history.len(), 2);
        assert!(!history.is_empty());
    }

    #[test]
    fn test_history_sorted_by_invoke_time() {
        let mut history = History::new();

        // Add operations out of order
        history.add(Operation::new(
            2,
            ClientId(1),
            "x".to_string(),
            OpKind::Read,
            Timestamp(300),
            Timestamp(400),
            OpResult::ReadOk(Some("a".to_string())),
        ));
        history.add(Operation::new(
            1,
            ClientId(1),
            "x".to_string(),
            OpKind::Write {
                value: "a".to_string(),
            },
            Timestamp(100),
            Timestamp(200),
            OpResult::WriteOk,
        ));

        let sorted = history.sorted_by_invoke_time();
        assert_eq!(sorted[0].id, 1);
        assert_eq!(sorted[1].id, 2);
    }

    #[test]
    fn test_history_successful_ops() {
        let mut history = History::new();

        history.add(Operation::new(
            1,
            ClientId(1),
            "x".to_string(),
            OpKind::Write {
                value: "a".to_string(),
            },
            Timestamp(100),
            Timestamp(200),
            OpResult::WriteOk,
        ));
        history.add(Operation::new(
            2,
            ClientId(1),
            "x".to_string(),
            OpKind::Read,
            Timestamp(300),
            Timestamp(400),
            OpResult::Error("timeout".to_string()),
        ));
        history.add(Operation::new(
            3,
            ClientId(1),
            "x".to_string(),
            OpKind::Read,
            Timestamp(500),
            Timestamp(600),
            OpResult::ReadOk(Some("a".to_string())),
        ));

        let successful = history.successful_ops();
        assert_eq!(successful.len(), 2);
        assert_eq!(successful[0].id, 1);
        assert_eq!(successful[1].id, 3);
    }
}
