//! In-memory storage implementation for testing
//!
//! Fast, no side effects, no persistence across restarts.
//! Perfect for unit tests.

use crate::core::raft_core::LogEntry;
use crate::core::snapshot::Snapshot;
use super::{Storage, StorageError};

/// In-memory storage implementation
///
/// Stores all state in memory - no disk I/O, no persistence.
/// Ideal for unit tests where speed matters and side effects are unwanted.
#[derive(Debug, Clone)]
pub struct MemoryStorage {
    term: u64,
    voted_for: Option<u64>,
    log: Vec<LogEntry>,
    snapshot: Option<Snapshot>,
}

impl MemoryStorage {
    /// Create a new empty in-memory storage
    pub fn new() -> Self {
        MemoryStorage {
            term: 0,
            voted_for: None,
            log: Vec::new(),
            snapshot: None,
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
        // Remove entries >= from_index, keep entries < from_index
        // Used for conflict resolution
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

    fn compact_log(&mut self, before_index: u64) -> Result<(), StorageError> {
        // Remove entries < before_index, keep entries >= before_index
        // Used for snapshot-based log compaction
        // Log entries are 1-indexed, so entry at index N is at position N-1
        if before_index > 1 {
            let remove_count = (before_index - 1) as usize;
            if remove_count < self.log.len() {
                self.log.drain(0..remove_count);
            } else {
                // before_index is beyond our log, clear everything
                self.log.clear();
            }
        } else if before_index == 1 {
            // Keep from index 1 onwards (keep everything)
            // No-op
        } else {
            // before_index 0 means clear everything
            self.log.clear();
        }
        Ok(())
    }

    fn load_snapshot(&self) -> Result<Option<Snapshot>, StorageError> {
        Ok(self.snapshot.clone())
    }

    fn save_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), StorageError> {
        self.snapshot = Some(snapshot.clone());
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

        // Truncate from index 3 (removes entries 3 and 4, keeps 1 and 2)
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

        // Truncate at index beyond log length - keeps all entries (no-op)
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

        // Truncate from index 1 (removes all entries >= 1, i.e., all entries)
        storage.truncate_log(1).unwrap();
        assert_eq!(storage.load_log().unwrap().len(), 0);
    }

    #[test]
    fn test_memory_storage_compact_log() {
        let mut storage = MemoryStorage::new();

        let entries = vec![
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
            LogEntry { term: 1, index: 3, command: "CMD 3".to_string() },
            LogEntry { term: 2, index: 4, command: "CMD 4".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();
        assert_eq!(storage.load_log().unwrap().len(), 4);

        // Compact from index 3 (removes entries 1 and 2, keeps 3 and 4)
        storage.compact_log(3).unwrap();
        let log = storage.load_log().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].index, 3);
        assert_eq!(log[1].index, 4);
    }

    #[test]
    fn test_memory_storage_snapshot() {
        use crate::core::snapshot::{Snapshot, SnapshotMetadata};

        let mut storage = MemoryStorage::new();

        // Initially no snapshot
        assert!(storage.load_snapshot().unwrap().is_none());

        // Save a snapshot
        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                last_included_index: 10,
                last_included_term: 2,
            },
            data: vec![1, 2, 3, 4],
        };
        storage.save_snapshot(&snapshot).unwrap();

        // Load it back
        let loaded = storage.load_snapshot().unwrap().unwrap();
        assert_eq!(loaded.metadata.last_included_index, 10);
        assert_eq!(loaded.metadata.last_included_term, 2);
        assert_eq!(loaded.data, vec![1, 2, 3, 4]);

        // Overwrite with new snapshot
        let snapshot2 = Snapshot {
            metadata: SnapshotMetadata {
                last_included_index: 20,
                last_included_term: 3,
            },
            data: vec![5, 6, 7],
        };
        storage.save_snapshot(&snapshot2).unwrap();

        let loaded = storage.load_snapshot().unwrap().unwrap();
        assert_eq!(loaded.metadata.last_included_index, 20);
        assert_eq!(loaded.metadata.last_included_term, 3);
    }
}
