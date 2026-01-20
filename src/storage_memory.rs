//! In-memory storage implementation for testing
//!
//! Fast, no side effects, no persistence across restarts.
//! Perfect for unit tests.

use crate::raft_core::LogEntry;
use crate::storage::{Storage, StorageError};

/// In-memory storage implementation
///
/// Stores all state in memory - no disk I/O, no persistence.
/// Ideal for unit tests where speed matters and side effects are unwanted.
#[derive(Debug, Clone)]
pub struct MemoryStorage {
    term: u64,
    voted_for: Option<u64>,
    log: Vec<LogEntry>,
}

impl MemoryStorage {
    /// Create a new empty in-memory storage
    pub fn new() -> Self {
        MemoryStorage {
            term: 0,
            voted_for: None,
            log: Vec::new(),
        }
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage for MemoryStorage {
    fn load_term(&self) -> Result<u64, StorageError> {
        Ok(self.term)
    }

    fn save_term(&mut self, term: u64) -> Result<(), StorageError> {
        self.term = term;
        Ok(())
    }

    fn load_voted_for(&self) -> Result<Option<u64>, StorageError> {
        Ok(self.voted_for)
    }

    fn save_voted_for(&mut self, voted_for: Option<u64>) -> Result<(), StorageError> {
        self.voted_for = voted_for;
        Ok(())
    }

    fn load_log(&self) -> Result<Vec<LogEntry>, StorageError> {
        Ok(self.log.clone())
    }

    fn append_log_entries(&mut self, entries: &[LogEntry]) -> Result<(), StorageError> {
        self.log.extend(entries.iter().cloned());
        Ok(())
    }

    fn truncate_log(&mut self, from_index: u64) -> Result<(), StorageError> {
        // Log entries are 1-indexed, so entry at index N is at position N-1
        if from_index > 0 {
            let truncate_pos = (from_index - 1) as usize;
            if truncate_pos < self.log.len() {
                self.log.truncate(truncate_pos);
            }
        } else {
            // from_index 0 means clear everything
            self.log.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_storage_term() {
        let mut storage = MemoryStorage::new();

        // Initial term is 0
        assert_eq!(storage.load_term().unwrap(), 0);

        // Save and load term
        storage.save_term(5).unwrap();
        assert_eq!(storage.load_term().unwrap(), 5);

        storage.save_term(10).unwrap();
        assert_eq!(storage.load_term().unwrap(), 10);
    }

    #[test]
    fn test_memory_storage_voted_for() {
        let mut storage = MemoryStorage::new();

        // Initial voted_for is None
        assert_eq!(storage.load_voted_for().unwrap(), None);

        // Save and load voted_for
        storage.save_voted_for(Some(3)).unwrap();
        assert_eq!(storage.load_voted_for().unwrap(), Some(3));

        // Clear voted_for
        storage.save_voted_for(None).unwrap();
        assert_eq!(storage.load_voted_for().unwrap(), None);
    }

    #[test]
    fn test_memory_storage_log_append() {
        let mut storage = MemoryStorage::new();

        // Initial log is empty
        assert_eq!(storage.load_log().unwrap().len(), 0);

        // Append entries
        let entries = vec![
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();

        let log = storage.load_log().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].command, "CMD 1");
        assert_eq!(log[1].command, "CMD 2");

        // Append more
        let more_entries = vec![
            LogEntry { term: 2, index: 3, command: "CMD 3".to_string() },
        ];
        storage.append_log_entries(&more_entries).unwrap();

        let log = storage.load_log().unwrap();
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn test_memory_storage_log_truncate() {
        let mut storage = MemoryStorage::new();

        // Add entries
        let entries = vec![
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
            LogEntry { term: 1, index: 3, command: "CMD 3".to_string() },
            LogEntry { term: 2, index: 4, command: "CMD 4".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();
        assert_eq!(storage.load_log().unwrap().len(), 4);

        // Truncate from index 3 (removes entries 3 and 4)
        storage.truncate_log(3).unwrap();
        let log = storage.load_log().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].index, 1);
        assert_eq!(log[1].index, 2);
    }

    #[test]
    fn test_memory_storage_truncate_beyond_log() {
        let mut storage = MemoryStorage::new();

        let entries = vec![
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();

        // Truncate at index beyond log length - should be no-op
        storage.truncate_log(10).unwrap();
        assert_eq!(storage.load_log().unwrap().len(), 1);
    }

    #[test]
    fn test_memory_storage_truncate_all() {
        let mut storage = MemoryStorage::new();

        let entries = vec![
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();

        // Truncate from index 1 (removes all)
        storage.truncate_log(1).unwrap();
        assert_eq!(storage.load_log().unwrap().len(), 0);
    }
}
