//! Unit tests for the WGL linearizability checker.
//!
//! These tests verify the checker itself works correctly using manually
//! constructed histories, without requiring a real cluster.

use chaos_test::{
    ClientId, History, LinearizabilityChecker, OpKind, OpResult, Operation, Timestamp,
};

/// Test that the checker correctly validates sequential operations.
#[test]
fn test_sequential_operations() {
    let mut history = History::new();

    // W("a") then R("a") - sequential, should be linearizable
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(200),
        Timestamp::from_micros(300),
        OpResult::ReadOk(Some("a".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
    assert_eq!(result.linearization, Some(vec![1, 2]));
}

/// Test that the checker correctly rejects stale reads.
#[test]
fn test_rejects_stale_read() {
    let mut history = History::new();

    // W("a") completes at t=100, R(None) starts at t=200
    // This is a stale read - after write completes, read should see "a"
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(200),
        Timestamp::from_micros(300),
        OpResult::ReadOk(None), // Stale! Should see "a"
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(!result.is_linearizable);
    assert!(result.error.is_some());
}

/// Test that the checker handles concurrent operations correctly.
#[test]
fn test_concurrent_operations() {
    let mut history = History::new();

    // Concurrent W("a") and W("b"), then R("b")
    // Valid linearization: W("a"), W("b"), R("b")
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(200),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(2),
        "x".to_string(),
        OpKind::Write {
            value: "b".to_string(),
        },
        Timestamp::from_micros(100),
        Timestamp::from_micros(300),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        3,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(400),
        Timestamp::from_micros(500),
        OpResult::ReadOk(Some("b".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
}

/// Test that the checker rejects impossible read values.
#[test]
fn test_rejects_impossible_read() {
    let mut history = History::new();

    // W("a"), W("b"), R("c") - "c" was never written!
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "b".to_string(),
        },
        Timestamp::from_micros(200),
        Timestamp::from_micros(300),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        3,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(400),
        Timestamp::from_micros(500),
        OpResult::ReadOk(Some("c".to_string())), // Impossible!
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(!result.is_linearizable);
}

/// Test that failed operations are filtered from linearizability check.
#[test]
fn test_filters_errors() {
    let mut history = History::new();

    // W("a") succeeds, R(error) fails, R("a") succeeds
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(150),
        Timestamp::from_micros(250),
        OpResult::Error("timeout".to_string()),
    ));
    history.add(Operation::new(
        3,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(300),
        Timestamp::from_micros(400),
        OpResult::ReadOk(Some("a".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);

    // Only ops 1 and 3 should be in linearization (op 2 had error)
    let lin = result.linearization.unwrap();
    assert_eq!(lin.len(), 2);
    assert!(lin.contains(&1));
    assert!(lin.contains(&3));
    assert!(!lin.contains(&2));
}

/// Test empty history is linearizable.
#[test]
fn test_empty_history() {
    let history = History::new();
    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
    assert_eq!(result.linearization, Some(vec![]));
}

/// Test initial read of None (before any writes).
#[test]
fn test_initial_read_none() {
    let mut history = History::new();

    // Read None when no writes have happened - valid
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::ReadOk(None),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
}

/// Sanity test: manually construct an invalid history to verify checker catches it.
/// This simulates a scenario where a read returns a value from the future.
#[test]
fn test_detects_future_read() {
    let mut history = History::new();

    // Timeline: R("b")[0,100] then W("a")[200,300] then W("b")[400,500]
    // The read at [0,100] returns "b", but W("b") doesn't start until t=400!
    // This is impossible and should fail.
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::ReadOk(Some("b".to_string())), // Reads "b" before it's written!
    ));
    history.add(Operation::new(
        2,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(200),
        Timestamp::from_micros(300),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        3,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "b".to_string(),
        },
        Timestamp::from_micros(400),
        Timestamp::from_micros(500),
        OpResult::WriteOk,
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(
        !result.is_linearizable,
        "Checker should detect impossible read from the future"
    );
}

/// Test read during overlapping write can see old value.
#[test]
fn test_read_during_write_sees_old() {
    let mut history = History::new();

    // W(a)[0,300] overlaps with R[100,200]
    // R returns None (old value) - valid if R linearizes before W
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(300),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(2),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(100),
        Timestamp::from_micros(200),
        OpResult::ReadOk(None),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
}

/// Test read during overlapping write can see new value.
#[test]
fn test_read_during_write_sees_new() {
    let mut history = History::new();

    // W(a)[0,300] overlaps with R[100,200]
    // R returns "a" (new value) - valid if W linearizes before R
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(300),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(2),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(100),
        Timestamp::from_micros(200),
        OpResult::ReadOk(Some("a".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
}

/// Test concurrent writes with read seeing first write's value.
#[test]
fn test_concurrent_writes_read_sees_first() {
    let mut history = History::new();

    // Two concurrent writes: W(a)[0,200] and W(b)[100,300]
    // Read sees "a" - valid if W(b) then W(a) (W(a) is last)
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Write {
            value: "a".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(200),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(2),
        "x".to_string(),
        OpKind::Write {
            value: "b".to_string(),
        },
        Timestamp::from_micros(100),
        Timestamp::from_micros(300),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        3,
        ClientId::new(1),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(400),
        Timestamp::from_micros(500),
        OpResult::ReadOk(Some("a".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
}

/// Test many concurrent writes to same key.
#[test]
fn test_many_concurrent_writes() {
    let mut history = History::new();

    // 5 concurrent writes, then read sees one of them
    for i in 0..5 {
        history.add(Operation::new(
            i,
            ClientId::new(i),
            "x".to_string(),
            OpKind::Write {
                value: format!("v{}", i),
            },
            Timestamp::from_micros(0),
            Timestamp::from_micros(100),
            OpResult::WriteOk,
        ));
    }
    // Read sees v3 - valid if v3 is last in linearization
    history.add(Operation::new(
        5,
        ClientId::new(0),
        "x".to_string(),
        OpKind::Read,
        Timestamp::from_micros(200),
        Timestamp::from_micros(300),
        OpResult::ReadOk(Some("v3".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
}

/// Test multiple keys are verified independently.
#[test]
fn test_multiple_keys_independent() {
    let mut history = History::new();

    // Key "a": write then read
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "a".to_string(),
        OpKind::Write {
            value: "val1".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(1),
        "a".to_string(),
        OpKind::Read,
        Timestamp::from_micros(200),
        Timestamp::from_micros(300),
        OpResult::ReadOk(Some("val1".to_string())),
    ));

    // Key "b": write then read
    history.add(Operation::new(
        3,
        ClientId::new(2),
        "b".to_string(),
        OpKind::Write {
            value: "val2".to_string(),
        },
        Timestamp::from_micros(50),
        Timestamp::from_micros(150),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        4,
        ClientId::new(2),
        "b".to_string(),
        OpKind::Read,
        Timestamp::from_micros(250),
        Timestamp::from_micros(350),
        OpResult::ReadOk(Some("val2".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(result.is_linearizable);
    assert_eq!(result.linearization.unwrap().len(), 4);
}

/// Test that failure on one key causes overall failure.
#[test]
fn test_multiple_keys_one_fails() {
    let mut history = History::new();

    // Key "a": valid - write then read
    history.add(Operation::new(
        1,
        ClientId::new(1),
        "a".to_string(),
        OpKind::Write {
            value: "val1".to_string(),
        },
        Timestamp::from_micros(0),
        Timestamp::from_micros(100),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        2,
        ClientId::new(1),
        "a".to_string(),
        OpKind::Read,
        Timestamp::from_micros(200),
        Timestamp::from_micros(300),
        OpResult::ReadOk(Some("val1".to_string())),
    ));

    // Key "b": INVALID - read value never written
    history.add(Operation::new(
        3,
        ClientId::new(2),
        "b".to_string(),
        OpKind::Write {
            value: "val2".to_string(),
        },
        Timestamp::from_micros(50),
        Timestamp::from_micros(150),
        OpResult::WriteOk,
    ));
    history.add(Operation::new(
        4,
        ClientId::new(2),
        "b".to_string(),
        OpKind::Read,
        Timestamp::from_micros(250),
        Timestamp::from_micros(350),
        OpResult::ReadOk(Some("never_written".to_string())),
    ));

    let result = LinearizabilityChecker::check(&history);
    assert!(!result.is_linearizable);
    assert!(result.error.as_ref().unwrap().contains("Key 'b'"));
}
