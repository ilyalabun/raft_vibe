//! Raft Consensus Algorithm - Learning Implementation
//!
//! This is an educational implementation of the Raft consensus protocol.

pub mod api;
pub mod core;
pub mod state_machine;
pub mod storage;
pub mod transport;

/// Testing utilities for integration tests.
pub mod testing;
