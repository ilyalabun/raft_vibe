//! Chaos testing framework for Raft clusters
//!
//! Provides Jepsen-like linearizability testing for the raft_vibe Raft implementation.
//! Uses the Wing-Gong Linearizability (WGL) algorithm to verify that concurrent
//! operations on a single-key register satisfy linearizability.

pub mod checker;
pub mod client;
pub mod docker;
pub mod history;
pub mod runner;

pub use checker::{CheckResult, LinearizabilityChecker};
pub use client::{ClientError, TestClient};
pub use history::{ClientId, History, OpKind, OpResult, Operation, Timestamp};
pub use runner::{run_test, TestConfig, TestResult};
