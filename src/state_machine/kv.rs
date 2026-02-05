//! Simple key-value store state machine
//!
//! Commands (state-changing operations that go through Raft):
//! - `SET key value` - Set a key to a value, returns Ok("")
//! - `DELETE key` - Delete a key, returns Ok("") or Err("NOT_FOUND")
//!
//! For reads, use the `get()` method directly (bypasses Raft log).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::core::raft_core::NOOP_COMMAND;
use super::{ApplyResult, Snapshotable, StateMachine};

/// Simple in-memory key-value store
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KeyValueStore {
    data: HashMap<String, String>,
}

impl KeyValueStore {
    pub fn new() -> Self {
        KeyValueStore {
            data: HashMap::new(),
        }
    }

    /// Get a value directly (for read-only queries, bypasses Raft)
    pub fn get(&self, key: &str) -> Option<String> {
        self.data.get(key).cloned()
    }

    /// Get all key-value pairs
    pub fn all(&self) -> HashMap<String, String> {
        self.data.clone()
    }
}

/// Shared key-value store that can be used for both Raft and direct reads
pub type SharedKvStore = Arc<Mutex<KeyValueStore>>;

impl StateMachine for SharedKvStore {
    fn validate(&self, command: &str) -> Result<(), String> {
        self.lock().unwrap().validate(command)
    }

    fn apply(&mut self, command: &str) -> ApplyResult {
        self.lock().unwrap().apply(command)
    }
}

impl Snapshotable for SharedKvStore {
    fn snapshot(&self) -> Result<Vec<u8>, String> {
        self.lock().unwrap().snapshot()
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), String> {
        self.lock().unwrap().restore(data)
    }
}

impl StateMachine for KeyValueStore {
    fn validate(&self, command: &str) -> Result<(), String> {
        // No-op command is always valid
        if command == NOOP_COMMAND {
            return Ok(());
        }

        let parts: Vec<&str> = command.splitn(3, ' ').collect();

        match parts.as_slice() {
            ["SET", _key, _value] => Ok(()),
            ["DELETE", _key] => Ok(()),
            _ => Err(format!("unknown command: {}", command)),
        }
    }

    fn apply(&mut self, command: &str) -> ApplyResult {
        // Handle no-op command (used by leader on election)
        if command == NOOP_COMMAND {
            return Ok(String::new());
        }

        let parts: Vec<&str> = command.splitn(3, ' ').collect();

        match parts.as_slice() {
            ["SET", key, value] => {
                self.data.insert(key.to_string(), value.to_string());
                Ok(String::new())
            }
            ["DELETE", key] => {
                if self.data.remove(*key).is_some() {
                    Ok(String::new())
                } else {
                    Err("NOT_FOUND".to_string())
                }
            }
            _ => Err(format!("unknown command: {}", command)),
        }
    }
}

impl Snapshotable for KeyValueStore {
    fn snapshot(&self) -> Result<Vec<u8>, String> {
        // Clone-then-serialize approach: clone the HashMap (cheap for small data),
        // then serialize without holding any locks
        let data_clone = self.data.clone();
        serde_json::to_vec(&data_clone).map_err(|e| format!("snapshot serialization failed: {}", e))
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), String> {
        let restored: HashMap<String, String> = serde_json::from_slice(data)
            .map_err(|e| format!("snapshot deserialization failed: {}", e))?;
        self.data = restored;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get() {
        let mut kv = KeyValueStore::new();

        let result = kv.apply("SET foo bar");
        assert!(result.is_ok());
        assert_eq!(kv.get("foo"), Some("bar".to_string()));
    }

    #[test]
    fn test_get_not_found() {
        let kv = KeyValueStore::new();
        assert_eq!(kv.get("nonexistent"), None);
    }

    #[test]
    fn test_delete() {
        let mut kv = KeyValueStore::new();

        kv.apply("SET foo bar").unwrap();

        let result = kv.apply("DELETE foo");
        assert!(result.is_ok());

        assert_eq!(kv.get("foo"), None);
    }

    #[test]
    fn test_delete_not_found() {
        let mut kv = KeyValueStore::new();

        let result = kv.apply("DELETE nonexistent");
        assert_eq!(result, Err("NOT_FOUND".to_string()));
    }

    #[test]
    fn test_overwrite() {
        let mut kv = KeyValueStore::new();

        kv.apply("SET key value1").unwrap();
        kv.apply("SET key value2").unwrap();

        assert_eq!(kv.get("key"), Some("value2".to_string()));
    }

    #[test]
    fn test_value_with_spaces() {
        let mut kv = KeyValueStore::new();

        // splitn(3, ' ') ensures value can contain spaces
        kv.apply("SET greeting hello world").unwrap();

        assert_eq!(kv.get("greeting"), Some("hello world".to_string()));
    }

    #[test]
    fn test_unknown_command() {
        let mut kv = KeyValueStore::new();

        let result = kv.apply("INVALID command");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown command"));
    }

    #[test]
    fn test_snapshot_and_restore() {
        let mut kv1 = KeyValueStore::new();
        kv1.apply("SET key1 value1").unwrap();
        kv1.apply("SET key2 value2").unwrap();
        kv1.apply("SET key3 value3").unwrap();

        // Take snapshot
        let snapshot = kv1.snapshot().unwrap();

        // Create new store and restore
        let mut kv2 = KeyValueStore::new();
        kv2.restore(&snapshot).unwrap();

        // Verify state matches
        assert_eq!(kv2.get("key1"), Some("value1".to_string()));
        assert_eq!(kv2.get("key2"), Some("value2".to_string()));
        assert_eq!(kv2.get("key3"), Some("value3".to_string()));
    }

    #[test]
    fn test_snapshot_empty_store() {
        let kv = KeyValueStore::new();
        let snapshot = kv.snapshot().unwrap();

        let mut kv2 = KeyValueStore::new();
        kv2.restore(&snapshot).unwrap();

        assert_eq!(kv2.all().len(), 0);
    }

    #[test]
    fn test_restore_overwrites_existing_data() {
        let mut kv1 = KeyValueStore::new();
        kv1.apply("SET original data").unwrap();
        let snapshot = kv1.snapshot().unwrap();

        let mut kv2 = KeyValueStore::new();
        kv2.apply("SET existing something").unwrap();
        kv2.apply("SET other thing").unwrap();

        // Restore should replace all existing data
        kv2.restore(&snapshot).unwrap();

        assert_eq!(kv2.get("original"), Some("data".to_string()));
        assert_eq!(kv2.get("existing"), None);
        assert_eq!(kv2.get("other"), None);
    }

    #[test]
    fn test_restore_invalid_data() {
        let mut kv = KeyValueStore::new();
        let result = kv.restore(b"invalid json data");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("deserialization failed"));
    }

    #[test]
    fn test_validate_set_command() {
        let kv = KeyValueStore::new();
        assert!(kv.validate("SET foo bar").is_ok());
        assert!(kv.validate("SET key value with spaces").is_ok());
    }

    #[test]
    fn test_validate_delete_command() {
        let kv = KeyValueStore::new();
        assert!(kv.validate("DELETE foo").is_ok());
    }

    #[test]
    fn test_validate_noop_command() {
        let kv = KeyValueStore::new();
        assert!(kv.validate(NOOP_COMMAND).is_ok());
    }

    #[test]
    fn test_validate_invalid_command() {
        let kv = KeyValueStore::new();

        // Unknown command
        let result = kv.validate("INVALID foo");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown command"));

        // SET without value
        let result = kv.validate("SET foo");
        assert!(result.is_err());

        // Empty command
        let result = kv.validate("");
        assert!(result.is_err());
    }
}
