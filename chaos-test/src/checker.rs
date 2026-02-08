//! Wing-Gong Linearizability (WGL) checker
//!
//! Implements the WGL algorithm for checking linearizability of concurrent operations
//! on a single-key register. Supports indeterminate (errored) writes that may or may
//! not have been committed — these are treated as optional in the linearization search.

use crate::history::{History, OpKind, OpResult, Operation, Timestamp};

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
    /// Check if multi-key history is linearizable
    ///
    /// Verifies linearizability for each key independently. Each key is treated
    /// as an independent single-key register.
    ///
    /// Errored write operations are treated as "indeterminate" — they may or may not
    /// have been committed. The checker will try to find a valid linearization both
    /// with and without these operations.
    pub fn check(history: &History) -> CheckResult {
        let keys: Vec<_> = history.unique_keys().into_iter().collect();

        if keys.is_empty() {
            return CheckResult::linearizable(vec![]);
        }

        let mut all_linearizations = Vec::new();

        for key in keys {
            let all_key_ops = history.ops_for_key(key);

            // Separate successful ops from indeterminate writes
            let key_ops: Vec<_> = all_key_ops
                .iter()
                .filter(|op| !matches!(op.result, OpResult::Error(_)))
                .copied()
                .collect();

            let indeterminate_writes: Vec<_> = all_key_ops
                .iter()
                .filter(|op| {
                    matches!((&op.kind, &op.result), (OpKind::Write { .. }, OpResult::Error(_)))
                })
                .copied()
                .collect();

            if key_ops.is_empty() && indeterminate_writes.is_empty() {
                continue;
            }

            let result = Self::check_single_key(&key_ops, &indeterminate_writes);

            if !result.is_linearizable {
                return CheckResult::not_linearizable(format!(
                    "Key '{}': {}",
                    key,
                    result.error.unwrap_or_default()
                ));
            }

            if let Some(lin) = result.linearization {
                all_linearizations.extend(lin);
            }
        }

        CheckResult::linearizable(all_linearizations)
    }

    /// Check if history is linearizable for a single-key register
    ///
    /// Uses the Wing-Gong algorithm:
    /// 1. Filter to only successful operations (indeterminate handled separately)
    /// 2. Quick check: all read values must have been written (or be None/initial)
    /// 3. Recursive search with backtracking
    /// 4. Track the "frontier" - earliest time at which next op can linearize
    ///
    /// Indeterminate writes are treated as optional: they may be included in the
    /// linearization (with their time interval extended to the end of the history)
    /// or skipped entirely.
    fn check_single_key(
        ops: &[&Operation],
        indeterminate_writes: &[&Operation],
    ) -> CheckResult {
        if ops.is_empty() && indeterminate_writes.is_empty() {
            return CheckResult::linearizable(vec![]);
        }

        // Fast check: collect all written values and verify reads only return those values.
        // Include indeterminate write values since they may have been committed.
        let mut written_values: std::collections::HashSet<Option<String>> =
            std::collections::HashSet::new();
        written_values.insert(None); // Initial state

        for op in ops {
            if let OpKind::Write { value } = &op.kind {
                written_values.insert(Some(value.clone()));
            }
        }
        for op in indeterminate_writes {
            if let OpKind::Write { value } = &op.kind {
                written_values.insert(Some(value.clone()));
            }
        }

        for op in ops {
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
        // If a read returns None (initial state) but started after a *confirmed* write completed,
        // it's a stale read. Skip this check for indeterminate writes (they may not have committed).
        for read_op in ops {
            if let (OpKind::Read, OpResult::ReadOk(None)) = (&read_op.kind, &read_op.result) {
                // Check if any confirmed write completed before this read started
                for write_op in ops {
                    if let OpKind::Write { value } = &write_op.kind {
                        if write_op.complete_ts.0 < read_op.invoke_ts.0 {
                            return CheckResult::not_linearizable(format!(
                                "Stale read: Read started at {} returned None, but write of {:?} completed at {}",
                                read_op.invoke_ts.0, value, write_op.complete_ts.0
                            ));
                        }
                    }
                }
            }
        }

        // Find the max timestamp across all operations to use as the extended
        // complete_ts for indeterminate writes
        let max_ts = ops
            .iter()
            .map(|op| op.complete_ts.0)
            .chain(indeterminate_writes.iter().map(|op| op.complete_ts.0))
            .max()
            .unwrap_or(0);

        // Create extended copies of indeterminate writes with complete_ts = max_ts.
        // These writes could have committed at any point after their invocation.
        let extended_indeterminate: Vec<Operation> = indeterminate_writes
            .iter()
            .map(|op| {
                Operation::new(
                    op.id,
                    op.client_id,
                    op.key.clone(),
                    op.kind.clone(),
                    op.invoke_ts,
                    Timestamp(max_ts),
                    OpResult::WriteOk, // Treat as if it succeeded for the model
                )
            })
            .collect();

        // Build the combined ops array: confirmed ops first, then indeterminate
        let confirmed_count = ops.len();
        let mut all_ops: Vec<&Operation> = ops.to_vec();
        for op in &extended_indeterminate {
            all_ops.push(op);
        }

        // Build list of remaining required operations (indices into all_ops for confirmed ops)
        let required: Vec<usize> = (0..confirmed_count).collect();
        // Indices for optional (indeterminate) ops
        let optional: Vec<usize> = (confirmed_count..all_ops.len()).collect();

        let model = RegisterModel::new();
        let mut linearization = Vec::new();

        // Start with frontier at 0 (any op can linearize from the beginning)
        if Self::search(
            &all_ops,
            required,
            optional,
            model,
            0,
            &mut linearization,
        ) {
            CheckResult::linearizable(linearization)
        } else {
            CheckResult::not_linearizable(
                "No valid linearization found: operations cannot be ordered to satisfy register semantics".to_string()
            )
        }
    }

    /// Recursive WGL search with optional operations
    ///
    /// Returns true if a valid linearization was found, false otherwise.
    /// The linearization order is accumulated in the `linearization` vector.
    ///
    /// `frontier` is the earliest time at which the next operation can linearize.
    ///
    /// `required` operations must all be placed. `optional` operations may be placed
    /// or skipped. The search succeeds when all required ops are placed.
    fn search(
        ops: &[&Operation],
        required: Vec<usize>,
        optional: Vec<usize>,
        model: RegisterModel,
        frontier: u64,
        linearization: &mut Vec<u64>,
    ) -> bool {
        // Success: all required operations have been placed
        if required.is_empty() {
            return true;
        }

        // Find the earliest complete_ts among remaining required ops.
        // Some operation MUST be linearized before this time, so only operations
        // invoked before this deadline are valid candidates.
        let min_complete = required
            .iter()
            .map(|&i| ops[i].complete_ts.0)
            .min()
            .unwrap_or(u64::MAX);

        let mut candidates: Vec<(usize, bool)> = Vec::new(); // (index, is_optional)

        for &i in &required {
            if ops[i].invoke_ts.0 <= min_complete && ops[i].complete_ts.0 >= frontier {
                candidates.push((i, false));
            }
        }
        for &i in &optional {
            if ops[i].invoke_ts.0 <= min_complete && ops[i].complete_ts.0 >= frontier {
                candidates.push((i, true));
            }
        }

        if candidates.is_empty() {
            return false;
        }

        // Sort candidates to try more promising orderings first:
        // 1. Operations with earlier invoke_ts
        // 2. Writes before reads
        candidates.sort_by(|&(a, _), &(b, _)| {
            let a_is_write = matches!(ops[a].kind, OpKind::Write { .. });
            let b_is_write = matches!(ops[b].kind, OpKind::Write { .. });
            ops[a]
                .invoke_ts
                .cmp(&ops[b].invoke_ts)
                .then_with(|| b_is_write.cmp(&a_is_write))
        });

        // Try each candidate
        for (candidate_idx, is_optional) in candidates {
            let op = ops[candidate_idx];

            // Check if this operation is valid given current model state
            let (is_valid, new_model) = Self::try_apply(op, &model);

            if is_valid {
                // Remove this operation from the appropriate set
                let new_required: Vec<usize> = if is_optional {
                    required.clone()
                } else {
                    required
                        .iter()
                        .copied()
                        .filter(|&i| i != candidate_idx)
                        .collect()
                };
                let new_optional: Vec<usize> = if is_optional {
                    optional
                        .iter()
                        .copied()
                        .filter(|&i| i != candidate_idx)
                        .collect()
                } else {
                    optional.clone()
                };

                // Add to linearization
                linearization.push(op.id);

                // Update frontier
                let new_frontier = frontier.max(op.invoke_ts.0);

                // Recurse
                if Self::search(
                    ops,
                    new_required,
                    new_optional,
                    new_model,
                    new_frontier,
                    linearization,
                ) {
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

    /// Helper to create a write operation for a specific key
    fn write_op_key(id: u64, key: &str, value: &str, invoke: u64, complete: u64) -> Operation {
        Operation::new(
            id,
            ClientId(1),
            key.to_string(),
            OpKind::Write {
                value: value.to_string(),
            },
            Timestamp(invoke),
            Timestamp(complete),
            OpResult::WriteOk,
        )
    }

    /// Helper to create a write operation (default key "x")
    fn write_op(id: u64, value: &str, invoke: u64, complete: u64) -> Operation {
        write_op_key(id, "x", value, invoke, complete)
    }

    /// Helper to create a read operation for a specific key
    fn read_op_key(id: u64, key: &str, value: Option<&str>, invoke: u64, complete: u64) -> Operation {
        Operation::new(
            id,
            ClientId(1),
            key.to_string(),
            OpKind::Read,
            Timestamp(invoke),
            Timestamp(complete),
            OpResult::ReadOk(value.map(|s| s.to_string())),
        )
    }

    /// Helper to create a read operation (default key "x")
    fn read_op(id: u64, value: Option<&str>, invoke: u64, complete: u64) -> Operation {
        read_op_key(id, "x", value, invoke, complete)
    }

    /// Helper to create an errored (indeterminate) write operation
    fn error_write_op(id: u64, value: &str, invoke: u64, complete: u64) -> Operation {
        Operation::new(
            id,
            ClientId(1),
            "x".to_string(),
            OpKind::Write {
                value: value.to_string(),
            },
            Timestamp(invoke),
            Timestamp(complete),
            OpResult::Error("connection lost".to_string()),
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
            "x".to_string(),
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

    // Multi-key tests

    #[test]
    fn test_multiple_keys_independent() {
        // Two keys with independent operations should both be linearizable
        let mut history = History::new();

        // Key "a": write then read
        history.add(write_op_key(1, "a", "val1", 0, 100));
        history.add(read_op_key(2, "a", Some("val1"), 200, 300));

        // Key "b": write then read
        history.add(write_op_key(3, "b", "val2", 50, 150));
        history.add(read_op_key(4, "b", Some("val2"), 250, 350));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
        let lin = result.linearization.unwrap();
        assert_eq!(lin.len(), 4);
    }

    #[test]
    fn test_multiple_keys_one_fails() {
        // One key is linearizable, another is not - should fail
        let mut history = History::new();

        // Key "a": valid - write then read
        history.add(write_op_key(1, "a", "val1", 0, 100));
        history.add(read_op_key(2, "a", Some("val1"), 200, 300));

        // Key "b": INVALID - read value never written
        history.add(write_op_key(3, "b", "val2", 50, 150));
        history.add(read_op_key(4, "b", Some("never_written"), 250, 350));

        let result = LinearizabilityChecker::check(&history);
        assert!(!result.is_linearizable);
        assert!(result.error.as_ref().unwrap().contains("Key 'b'"));
    }

    #[test]
    fn test_multiple_keys_concurrent() {
        // Concurrent operations across multiple keys
        let mut history = History::new();

        // All operations overlap in time
        history.add(write_op_key(1, "x", "a", 0, 200));
        history.add(write_op_key(2, "y", "b", 50, 250));
        history.add(read_op_key(3, "x", Some("a"), 100, 300));
        history.add(read_op_key(4, "y", Some("b"), 150, 350));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
    }

    #[test]
    fn test_multiple_keys_stale_read_on_one() {
        // Stale read on one key should fail the whole check
        let mut history = History::new();

        // Key "a": valid
        history.add(write_op_key(1, "a", "val1", 0, 100));
        history.add(read_op_key(2, "a", Some("val1"), 200, 300));

        // Key "b": STALE READ - write completed, then read returns None
        history.add(write_op_key(3, "b", "val2", 0, 100));
        history.add(read_op_key(4, "b", None, 200, 300)); // Stale!

        let result = LinearizabilityChecker::check(&history);
        assert!(!result.is_linearizable);
        assert!(result.error.as_ref().unwrap().contains("Key 'b'"));
        assert!(result.error.as_ref().unwrap().contains("Stale read"));
    }

    #[test]
    fn test_three_keys_all_valid() {
        // Three independent keys, all linearizable
        let mut history = History::new();

        // Key "k1"
        history.add(write_op_key(1, "k1", "v1", 0, 100));
        history.add(read_op_key(2, "k1", Some("v1"), 200, 300));

        // Key "k2"
        history.add(write_op_key(3, "k2", "v2", 0, 100));
        history.add(write_op_key(4, "k2", "v3", 200, 300));
        history.add(read_op_key(5, "k2", Some("v3"), 400, 500));

        // Key "k3"
        history.add(read_op_key(6, "k3", None, 0, 100)); // Read initial None
        history.add(write_op_key(7, "k3", "v4", 200, 300));
        history.add(read_op_key(8, "k3", Some("v4"), 400, 500));

        let result = LinearizabilityChecker::check(&history);
        assert!(result.is_linearizable);
        assert_eq!(result.linearization.unwrap().len(), 8);
    }

    // Indeterminate write tests

    #[test]
    fn test_indeterminate_write_value_read_later() {
        // Scenario: Write "a" errors (indeterminate), but later read sees "a".
        // This is valid because the write may have been committed.
        let mut history = History::new();
        history.add(error_write_op(1, "a", 0, 100)); // Indeterminate
        history.add(read_op(2, Some("a"), 200, 300)); // Sees "a"

        let result = LinearizabilityChecker::check(&history);
        assert!(
            result.is_linearizable,
            "Indeterminate write followed by read of that value should be linearizable: {:?}",
            result.error
        );
    }

    #[test]
    fn test_indeterminate_write_not_read() {
        // Scenario: Write "a" errors, but later read sees None (initial state).
        // This is valid because the write may NOT have been committed.
        let mut history = History::new();
        history.add(error_write_op(1, "a", 0, 100)); // Indeterminate
        history.add(read_op(2, None, 200, 300)); // Sees None

        let result = LinearizabilityChecker::check(&history);
        assert!(
            result.is_linearizable,
            "Indeterminate write followed by read of None should be linearizable: {:?}",
            result.error
        );
    }

    #[test]
    fn test_indeterminate_write_then_confirmed_write() {
        // Scenario: Indeterminate write "a", then confirmed write "b", then read "b".
        // Valid: the indeterminate write may or may not have happened,
        // but the confirmed write "b" is the latest.
        let mut history = History::new();
        history.add(error_write_op(1, "a", 0, 100)); // Indeterminate
        history.add(write_op(2, "b", 200, 300)); // Confirmed
        history.add(read_op(3, Some("b"), 400, 500)); // Sees "b"

        let result = LinearizabilityChecker::check(&history);
        assert!(
            result.is_linearizable,
            "Should be linearizable: {:?}",
            result.error
        );
    }

    #[test]
    fn test_indeterminate_write_overrides_confirmed() {
        // Scenario: Confirmed write "a", then indeterminate write "b", then read "b".
        // Valid: the indeterminate write may have committed after the confirmed one.
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100)); // Confirmed
        history.add(error_write_op(2, "b", 200, 300)); // Indeterminate
        history.add(read_op(3, Some("b"), 400, 500)); // Sees "b"

        let result = LinearizabilityChecker::check(&history);
        assert!(
            result.is_linearizable,
            "Indeterminate write overriding confirmed should be linearizable: {:?}",
            result.error
        );
    }

    #[test]
    fn test_indeterminate_write_does_not_excuse_impossible_read() {
        // Scenario: Confirmed write "a", read "c" (never written, not even indeterminate).
        // Should still fail.
        let mut history = History::new();
        history.add(write_op(1, "a", 0, 100));
        history.add(error_write_op(2, "b", 200, 300)); // Indeterminate but writes "b"
        history.add(read_op(3, Some("c"), 400, 500)); // "c" never written

        let result = LinearizabilityChecker::check(&history);
        assert!(!result.is_linearizable);
    }

    #[test]
    fn test_multiple_indeterminate_writes() {
        // Multiple indeterminate writes with a read that sees one of them
        let mut history = History::new();
        history.add(error_write_op(1, "a", 0, 100)); // Indeterminate
        history.add(error_write_op(2, "b", 50, 150)); // Indeterminate
        history.add(read_op(3, Some("b"), 200, 300)); // Sees "b"

        let result = LinearizabilityChecker::check(&history);
        assert!(
            result.is_linearizable,
            "Multiple indeterminate writes should be linearizable: {:?}",
            result.error
        );
    }
}
