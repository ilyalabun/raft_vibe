//! State machine layer for Raft
//!
//! - `KeyValueStore`: Simple key-value store
//! - `TestStateMachine`: Records commands for testing

pub mod kv;
pub mod traits;

pub use traits::{AppliedCommands, ApplyResult, StateMachine, TestStateMachine};
