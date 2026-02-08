//! Wing-Gong Linearizability (WGL) checker
//!
//! Implements the WGL algorithm for checking linearizability of concurrent operations
//! on a single-key register.

use crate::history::{History, OpKind, OpResult, Operation};

/// Result of linearizability check
#[derive(Debug)]
pub struct CheckResult {
    /// Whether the history is linearizable
    pub is_linearizable: bool,
    /// Operation IDs in valid linearization order (if found)
    pub linearization: Option<Vec<u64>>,
    /// Description if not linearizable
    pub error: Option<String>,
}

impl CheckResult {
    fn linearizable(order: Vec<u64>) -> Self {
        CheckResult {
            is_linearizable: true,
            linearization: Some(order),
            error: None,
        }
    }

    fn not_linearizable(reason: String) -> Self {
        CheckResult {
            is_linearizable: false,
            linearization: None,
            error: Some(reason),
        }
    }
}

/// Single-key register model for WGL
#[derive(Clone, Debug)]
struct RegisterModel {
    /// Current value (None = key doesn't exist / initial state)
    value: Option<String>,
}

impl RegisterModel {
    fn new() -> Self {
        RegisterModel { value: None }
    }

    fn apply_write(&mut self, value: &str) {
        self.value = Some(value.to_string());
    }

    fn check_read(&self, expected: &Option<String>) -> bool {
        self.value == *expected
    }
}

/// WGL linearizability checker
pub struct LinearizabilityChecker;

impl LinearizabilityChecker {
    /// Check if history is linearizable for a single-key register
    ///
    /// Uses the Wing-Gong algorithm:
    /// 1. Filter to only successful operations
    /// 2. Quick check: all read values must have been written (or be None/initial)
    /// 3. Recursive search with backtracking
    /// 4. Track the "frontier" - earliest time at which next op can linearize
    pub fn check(history: &History) -> CheckResult {
        // Get only successful operations
        let ops: Vec<_> = history.successful_ops();

        if ops.is_empty() {
            return CheckResult::linearizable(vec![]);
        }

        // Fast check: collect all written values and verify reads only return those values
        let mut written_values: std::collections::HashSet<Option<String>> =
            std::collections::HashSet::new();
        written_values.insert(None); // Initial state

        for op in &ops {
            if let OpKind::Write { value } = &op.kind {
                written_values.insert(Some(value.clone()));
            }
        }

        for op in &ops {
            if let (OpKind::Read, OpResult::ReadOk(read_value)) = (&op.kind, &op.result) {
                if !written_values.contains(read_value) {
                    return CheckResult::not_linearizable(format!(
                        "Read returned value {:?} which was never written",
                        read_value
                    ));
                }
            }
        }

        // Fast check for obvious stale reads:
        // If a read returns None (initial state) but started after any write completed,
        // and no later writes could have "undone" the value, it's a stale read.
        // (For a single-key register without DELETE, once written, reading None is stale)
        for read_op in &ops {
            if let (OpKind::Read, OpResult::ReadOk(None)) = (&read_op.kind, &read_op.result) {
                // Check if any write completed before this read started
                for write_op in &ops {
                    if let OpKind::Write { value } = &write_op.kind {
                        // Write completed before read started - read should not see None
                        if write_op.complete_ts.0 < read_op.invoke_ts.0 {
                            // Check if there's any way the read could validly see None:
                            // Only if there's a concurrent write that could linearize before
                            // the first write. But if the write completed, it must linearize
                            // before its complete_ts.
                            return CheckResult::not_linearizable(format!(
                                "Stale read: Read started at {} returned None, but write of {:?} completed at {}",
                                read_op.invoke_ts.0, value, write_op.complete_ts.0
                            ));
                        }
                    }
                }
            }
        }

        // Build list of remaining operations (indices into ops)
        let remaining: Vec<usize> = (0..ops.len()).collect();

        let model = RegisterModel::new();
        let mut linearization = Vec::new();

        // Start with frontier at 0 (any op can linearize from the beginning)
        if Self::search(&ops, remaining, model, 0, &mut linearization) {
            CheckResult::linearizable(linearization)
        } else {
            CheckResult::not_linearizable(
                "No valid linearization found: operations cannot be ordered to satisfy register semantics".to_string()
            )
        }
    }

    /// Recursive WGL search
    ///
    /// Returns true if a valid linearization was found, false otherwise.
    /// The linearization order is accumulated in the `linearization` vector.
    ///
    /// `frontier` is the earliest time at which the next operation can linearize.
    /// This is at least the invoke_ts of all previously linearized operations.
    fn search(
        ops: &[&Operation],
        remaining: Vec<usize>,
        model: RegisterModel,
        frontier: u64,
        linearization: &mut Vec<u64>,
    ) -> bool {
        if remaining.is_empty() {
            return true;
        }

        // Find all operations that could linearize next.
        // An operation can linearize if there exists a time t such that:
        // - t >= frontier (after all previously linearized operations)
        // - t is in [invoke_ts, complete_ts] (within the operation's interval)
        // This is equivalent to: complete_ts >= frontier (since we can pick t = max(invoke_ts, frontier))
        let candidates: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| ops[i].complete_ts.0 >= frontier)
            .collect();

        if candidates.is_empty() {
            return false;
        }

        // Sort candidates to try more promising orderings first:
        // 1. Operations with earlier invoke_ts (they "should" linearize earlier)
        // 2. Writes before reads (writes enable more reads to succeed)
        let mut sorted_candidates = candidates;
        sorted_candidates.sort_by(|&a, &b| {
            let a_is_write = matches!(ops[a].kind, OpKind::Write { .. });
            let b_is_write = matches!(ops[b].kind, OpKind::Write { .. });
            // First by invoke time, then writes before reads
            ops[a].invoke_ts.cmp(&ops[b].invoke_ts)
                .then_with(|| b_is_write.cmp(&a_is_write))
        });

        // Try each candidate
        for candidate_idx in sorted_candidates {
            let op = ops[candidate_idx];

            // Check if this operation is valid given current model state
            let (is_valid, new_model) = Self::try_apply(op, &model);

            if is_valid {
                // Remove this operation from remaining
                let new_remaining: Vec<usize> = remaining
                    .iter()
                    .copied()
                    .filter(|&i| i != candidate_idx)
                    .collect();

                // Add to linearization
                linearization.push(op.id);

                // Update frontier: the next op must linearize at or after this op's invoke time
                // (Since this op linearizes at some point >= max(frontier, op.invoke_ts))
                let new_frontier = frontier.max(op.invoke_ts.0);

                // Recurse
                if Self::search(ops, new_remaining, new_model, new_frontier, linearization) {
                    return true;
                }

                // Backtrack
                linearization.pop();
            }
        }

        false
    }

    /// Try to apply an operation to the model
    ///
    /// Returns (is_valid, new_model) where:
    /// - For writes: always valid, updates model state
    /// - For reads: valid if read value matches model state
    fn try_apply(op: &Operation, model: &RegisterModel) -> (bool, RegisterModel) {
        match (&op.kind, &op.result) {
            (OpKind::Write { value }, OpResult::WriteOk) => {
                let mut new_model = model.clone();
                new_model.apply_write(value);
                (true, new_model)
            }
            (OpKind::Read, OpResult::ReadOk(read_value)) => {
                if model.check_read(read_value) {
                    (true, model.clone())
                } else {
                    (false, model.clone())
                }
            }
            // Errors shouldn't reach here (filtered out), but handle gracefully
            _ => (false, model.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{ClientId, History, OpKind, OpResult, Operation, Timestamp};

    /// Helper to create a write operation
    fn write_op(id: u64, value: &str, invoke: u64, complete: u64) -> Operation {
        Operation::new(
            id,
            ClientId(1),
            OpKind::Write {
                value: value.to_string(),
            },
            Timestamp(invoke),
            Timestamp(complete),
            OpResult::WriteOk,
        )
    }

    /// Helper to create a read operation
    fn read_op(id: u64, value: Option<&str>, invoke: u64, complete: u64) -> Operation {
        Operation::new(
            id,
            ClientId(1),
            OpKind::Read,
            Timestamp(invoke),
            Timestamp(complete),
            OpResult::ReadOk(value.map(|s| s.to_string())),
        )
    }

    #[test]
    fn test_empty_history() {
        let history = History::new();
        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
        assert_eq!(result.linearization, Some(vec![]));
    }

    #[test]
    fn test_sequential_ops_linearizable() {
        // Sequential operations: write "a", then read "a"
        // Timeline: W(a)[0,100] --> R(a)[200,300]
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100));
        history.add(read_op(2, Some("a"), 200, 300));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
        assert_eq!(result.linearization, Some(vec![1, 2]));
    }

    #[test]
    fn test_concurrent_ops_valid_linearization() {
        // Overlapping operations with valid linearization
        // W(a)[0,200] overlaps with R(a)[100,300]
        // Valid linearization: W(a) then R(a) (read sees "a")
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 200));
        history.add(read_op(2, Some("a"), 100, 300));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
        // Write must come before read for read to see "a"
        assert_eq!(result.linearization, Some(vec![1, 2]));
    }

    #[test]
    fn test_read_initial_state() {
        // Read before any writes should see None
        let mut history = History::new();
        history.add(read_op(1, None, 0, 100));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_stale_read_not_linearizable() {
        // Write completes, then read starts and returns old value (None)
        // This is a stale read and should not be linearizable
        // Timeline: W(a)[0,100] --> R(None)[200,300]
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100));
        history.add(read_op(2, None, 200, 300)); // Reading None after write completed!

        let result = LinearizabilityChecker::check(&history);
        assert!(!result.is_linearizable);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_multiple_writes_overwrite() {
        // W(a) then W(b) then R(b) - should be linearizable
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100));
        history.add(write_op(2, "b", 200, 300));
        history.add(read_op(3, Some("b"), 400, 500));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_concurrent_writes_both_orders_possible() {
        // Two concurrent writes: W(a)[0,200] and W(b)[100,300]
        // Then read sees "b"
        // Valid if W(a) then W(b) then R(b)
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 200));
        history.add(write_op(2, "b", 100, 300));
        history.add(read_op(3, Some("b"), 400, 500));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_concurrent_writes_read_sees_first() {
        // Two concurrent writes: W(a)[0,200] and W(b)[100,300]
        // Read sees "a" - valid if W(b) then W(a)
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 200));
        history.add(write_op(2, "b", 100, 300));
        history.add(read_op(3, Some("a"), 400, 500));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_impossible_read_value() {
        // Write "a", write "b", read "c" - impossible
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100));
        history.add(write_op(2, "b", 200, 300));
        history.add(read_op(3, Some("c"), 400, 500)); // "c" was never written!

        let result = LinearizabilityChecker::check(&history);
        assert!(!result.is_linearizable);
    }

    #[test]
    fn test_concurrent_read_during_write() {
        // Read overlaps with write, can see either old or new value
        // W(a)[0,300] overlaps R[100,200]
        // If read sees None (old value), valid: R linearizes before W
        // If read sees "a" (new value), valid: W linearizes before R
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 300));
        history.add(read_op(2, None, 100, 200)); // Sees old value

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_concurrent_read_during_write_sees_new() {
        // Same as above but read sees new value
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 300));
        history.add(read_op(2, Some("a"), 100, 200)); // Sees new value

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_complex_concurrent_scenario() {
        // More complex scenario with multiple concurrent ops
        // W(a)[0,200], W(b)[100,300], R1[150,250], R2[350,450]
        // R1 could see None, "a", or "b" depending on linearization
        // R2 must see either "a" or "b" (both writes complete before R2)
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 200));
        history.add(write_op(2, "b", 100, 300));
        history.add(read_op(3, Some("a"), 150, 250)); // Sees "a"
        history.add(read_op(4, Some("b"), 350, 450)); // Sees "b"

        // This is NOT linearizable because:
        // - R1 sees "a" means W(a) linearizes before R1
        // - R2 sees "b" means W(b) linearizes after W(a) (so R2 sees the later value)
        // - But R1 linearizes during [150,250] and must see the value at that point
        // - If W(b) linearizes after R1 and before R2, this works!
        // Valid order: W(a), R1(a), W(b), R2(b)
        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_read_between_writes_inconsistent() {
        // Sequential: W(a)[0,100], R(b)[200,300], W(b)[400,500]
        // Read sees "b" but W(b) hasn't happened yet - impossible!
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100));
        history.add(read_op(2, Some("b"), 200, 300)); // Sees "b"
        history.add(write_op(3, "b", 400, 500));

        let result = LinearizabilityChecker::check(&history);
        assert!(!result.is_linearizable);
    }

    #[test]
    fn test_error_operations_filtered() {
        // Operations with errors should be filtered out
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100));
        history.add(Operation::new(
            2,
            ClientId(1),
            OpKind::Read,
            Timestamp(200),
            Timestamp(300),
            OpResult::Error("timeout".to_string()),
        ));
        history.add(read_op(3, Some("a"), 400, 500));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
        // Only ops 1 and 3 should be in linearization
        let lin = result.linearization.unwrap();
        assert_eq!(lin.len(), 2);
        assert!(lin.contains(&1));
        assert!(lin.contains(&3));
    }

    #[test]
    fn test_many_concurrent_writes_same_key() {
        // Many concurrent writes, then read sees one of them
        let mut history = History::new();
        for i in 0..5 {
            history.add(write_op(i, &format!("v{}", i), 0, 100));
        }
        // Read sees v3 - valid as long as v3 is the last write in linearization
        history.add(read_op(5, Some("v3"), 200, 300));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
        // v3 should be last write before read
        let lin = result.linearization.unwrap();
        // Find position of write 3 and read 5
        let w3_pos = lin.iter().position(|&id| id == 3).unwrap();
        let r5_pos = lin.iter().position(|&id| id == 5).unwrap();
        // w3 must be the last write before r5
        assert!(w3_pos < r5_pos);
        // No other writes between w3 and r5
        for i in (w3_pos + 1)..r5_pos {
            let op_id = lin[i];
            // op_id should not be a write (0-4)
            assert!(op_id >= 5 || op_id == 3);
        }
    }
}
