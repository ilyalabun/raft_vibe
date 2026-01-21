//! Simple key-value store state machine
//!
//! Commands (state-changing operations that go through Raft):
//! - `SET key value` - Set a key to a value, returns Ok("")
//! - `DELETE key` - Delete a key, returns Ok("") or Err("NOT_FOUND")
//!
//! For reads, use the `get()` method directly (bypasses Raft log).

use std::collections::HashMap;

use crate::state_machine::{ApplyResult, StateMachine};

/// Simple in-memory key-value store
#[derive(Debug, Default)]
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
    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.data.get(key).map(|s| s.as_str())
    }
}

impl StateMachine for KeyValueStore {
    fn apply(&mut self, command: &str) -> ApplyResult {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get() {
        let mut kv = KeyValueStore::new();

        let result = kv.apply("SET foo bar");
        assert!(result.is_ok());
        assert_eq!(kv.get("foo"), Some("bar"));
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

        assert_eq!(kv.get("key"), Some("value2"));
    }

    #[test]
    fn test_value_with_spaces() {
        let mut kv = KeyValueStore::new();

        // splitn(3, ' ') ensures value can contain spaces
        kv.apply("SET greeting hello world").unwrap();

        assert_eq!(kv.get("greeting"), Some("hello world"));
    }

    #[test]
    fn test_unknown_command() {
        let mut kv = KeyValueStore::new();

        let result = kv.apply("INVALID command");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown command"));
    }
}
