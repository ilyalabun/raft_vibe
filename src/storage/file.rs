//! File-based storage implementation for Raft persistent state
//!
//! Stores state in four files within a directory:
//! - `term` - Current term (u64) with checksum
//! - `voted_for` - Voted for candidate ID (Option<u64>) with checksum
//! - `log` - Log entries (JSON lines format, each line has checksum)
//! - `snapshot` - Most recent snapshot (JSON with checksum)
//!
//! Uses checksums to detect corruption from partial writes.

use crate::core::raft_core::LogEntry;
use crate::core::snapshot::Snapshot;
use super::{Storage, StorageError};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Simple CRC32 checksum (IEEE polynomial)
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// File-based storage implementation
pub struct FileStorage {
    dir: PathBuf,
}

impl FileStorage {
    /// Create a new FileStorage in the given directory
    /// Creates the directory if it doesn't exist
    pub fn new<P: AsRef<Path>>(dir: P) -> Result<Self, StorageError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(FileStorage { dir })
    }

    fn term_path(&self) -> PathBuf {
        self.dir.join("term")
    }

    fn voted_for_path(&self) -> PathBuf {
        self.dir.join("voted_for")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("log")
    }

    fn snapshot_path(&self) -> PathBuf {
        self.dir.join("snapshot")
    }

    /// Write data with checksum: "{data} {crc32_hex}\n"
    fn write_with_checksum(&self, path: &Path, data: &str) -> Result<(), StorageError> {
        let checksum = crc32(data.as_bytes());
        let content = format!("{} {:08x}\n", data, checksum);

        let mut file = File::create(path).map_err(|e| StorageError::Io(e.to_string()))?;
        file.write_all(content.as_bytes())
            .map_err(|e| StorageError::Io(e.to_string()))?;
        file.sync_all()
            .map_err(|e| StorageError::Io(e.to_string()))?;

        Ok(())
    }

    /// Read and verify checksum, returns the data portion
    fn read_with_checksum(&self, path: &Path) -> Result<Option<String>, StorageError> {
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(path).map_err(|e| StorageError::Io(e.to_string()))?;
        let content = content.trim();

        if content.is_empty() {
            return Ok(None);
        }

        // Parse "{data} {checksum}"
        let parts: Vec<&str> = content.rsplitn(2, ' ').collect();
        if parts.len() != 2 {
            return Err(StorageError::Corruption(format!(
                "invalid format in {:?}: missing checksum",
                path
            )));
        }

        let checksum_str = parts[0];
        let data = parts[1];

        let stored_checksum = u32::from_str_radix(checksum_str, 16).map_err(|_| {
            StorageError::Corruption(format!("invalid checksum format in {:?}", path))
        })?;

        let computed_checksum = crc32(data.as_bytes());

        if stored_checksum != computed_checksum {
            return Err(StorageError::Corruption(format!(
                "checksum mismatch in {:?}: stored {:08x}, computed {:08x}",
                path, stored_checksum, computed_checksum
            )));
        }

        Ok(Some(data.to_string()))
    }

    /// Atomically write data to a file (write to temp, fsync, rename)
    /// Used for log truncation where we rewrite the entire file
    fn atomic_write(&self, path: &Path, data: &[u8]) -> Result<(), StorageError> {
        let temp_path = path.with_extension("tmp");

        let mut file = File::create(&temp_path).map_err(|e| StorageError::Io(e.to_string()))?;
        file.write_all(data)
            .map_err(|e| StorageError::Io(e.to_string()))?;
        file.sync_all()
            .map_err(|e| StorageError::Io(e.to_string()))?;

        fs::rename(&temp_path, path).map_err(|e| StorageError::Io(e.to_string()))?;

        Ok(())
    }
}

impl Storage for FileStorage {
    fn load_term(&self) -> Result<u64, StorageError> {
        match self.read_with_checksum(&self.term_path())? {
            None => Ok(0),
            Some(data) => data
                .parse()
                .map_err(|e| StorageError::Corruption(format!("invalid term: {}", e))),
        }
    }

    fn save_term(&mut self, term: u64) -> Result<(), StorageError> {
        self.write_with_checksum(&self.term_path(), &term.to_string())
    }

    fn load_voted_for(&self) -> Result<Option<u64>, StorageError> {
        match self.read_with_checksum(&self.voted_for_path())? {
            None => Ok(None),
            Some(data) if data == "none" => Ok(None),
            Some(data) => {
                let id = data
                    .parse()
                    .map_err(|e| StorageError::Corruption(format!("invalid voted_for: {}", e)))?;
                Ok(Some(id))
            }
        }
    }

    fn save_voted_for(&mut self, voted_for: Option<u64>) -> Result<(), StorageError> {
        let data = match voted_for {
            Some(id) => id.to_string(),
            None => "none".to_string(),
        };
        self.write_with_checksum(&self.voted_for_path(), &data)
    }

    fn load_log(&self) -> Result<Vec<LogEntry>, StorageError> {
        let path = self.log_path();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&path).map_err(|e| StorageError::Io(e.to_string()))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| StorageError::Io(e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }

            // Each line: "{json} {checksum}"
            let parts: Vec<&str> = line.rsplitn(2, ' ').collect();
            if parts.len() != 2 {
                return Err(StorageError::Corruption(format!(
                    "invalid log format at line {}: missing checksum",
                    line_num + 1
                )));
            }

            let checksum_str = parts[0];
            let json = parts[1];

            // Verify checksum
            let stored_checksum = u32::from_str_radix(checksum_str, 16).map_err(|_| {
                StorageError::Corruption(format!(
                    "invalid checksum format at line {}",
                    line_num + 1
                ))
            })?;

            let computed_checksum = crc32(json.as_bytes());
            if stored_checksum != computed_checksum {
                return Err(StorageError::Corruption(format!(
                    "checksum mismatch at line {}: stored {:08x}, computed {:08x}",
                    line_num + 1,
                    stored_checksum,
                    computed_checksum
                )));
            }

            let entry: LogEntry = serde_json::from_str(json).map_err(|e| {
                StorageError::Corruption(format!(
                    "invalid log entry at line {}: {}",
                    line_num + 1,
                    e
                ))
            })?;
            entries.push(entry);
        }

        Ok(entries)
    }

    fn append_log_entries(&mut self, entries: &[LogEntry]) -> Result<(), StorageError> {
        let path = self.log_path();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| StorageError::Io(e.to_string()))?;

        for entry in entries {
            let json = serde_json::to_string(entry)
                .map_err(|e| StorageError::Io(format!("serialization error: {}", e)))?;
            let checksum = crc32(json.as_bytes());
            writeln!(file, "{} {:08x}", json, checksum)
                .map_err(|e| StorageError::Io(e.to_string()))?;
        }

        file.sync_all()
            .map_err(|e| StorageError::Io(e.to_string()))?;

        Ok(())
    }

    fn truncate_log(&mut self, from_index: u64) -> Result<(), StorageError> {
        // Remove entries >= from_index, keep entries < from_index
        // Used for conflict resolution
        let entries = self.load_log()?;
        let keep: Vec<_> = entries
            .into_iter()
            .filter(|e| e.index < from_index)
            .collect();

        // Rewrite the log file with checksums
        let path = self.log_path();
        let mut content = String::new();
        for entry in &keep {
            let json = serde_json::to_string(entry)
                .map_err(|e| StorageError::Io(format!("serialization error: {}", e)))?;
            let checksum = crc32(json.as_bytes());
            content.push_str(&format!("{} {:08x}\n", json, checksum));
        }

        self.atomic_write(&path, content.as_bytes())
    }

    fn compact_log(&mut self, before_index: u64) -> Result<(), StorageError> {
        // Remove entries < before_index, keep entries >= before_index
        // Used for snapshot-based log compaction
        let entries = self.load_log()?;
        let keep: Vec<_> = entries
            .into_iter()
            .filter(|e| e.index >= before_index)
            .collect();

        // Rewrite the log file with checksums
        let path = self.log_path();
        let mut content = String::new();
        for entry in &keep {
            let json = serde_json::to_string(entry)
                .map_err(|e| StorageError::Io(format!("serialization error: {}", e)))?;
            let checksum = crc32(json.as_bytes());
            content.push_str(&format!("{} {:08x}\n", json, checksum));
        }

        self.atomic_write(&path, content.as_bytes())
    }

    fn load_snapshot(&self) -> Result<Option<Snapshot>, StorageError> {
        let path = self.snapshot_path();
        match self.read_with_checksum(&path)? {
            None => Ok(None),
            Some(json) => {
                let snapshot: Snapshot = serde_json::from_str(&json)
                    .map_err(|e| StorageError::Corruption(format!("invalid snapshot: {}", e)))?;
                Ok(Some(snapshot))
            }
        }
    }

    fn save_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), StorageError> {
        let json = serde_json::to_string(snapshot)
            .map_err(|e| StorageError::Io(format!("snapshot serialization error: {}", e)))?;
        self.write_with_checksum(&self.snapshot_path(), &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_storage() -> (FileStorage, TempDir) {
        let dir = TempDir::new().unwrap();
        let storage = FileStorage::new(dir.path()).unwrap();
        (storage, dir)
    }

    #[test]
    fn test_file_storage_term() {
        let (mut storage, _dir) = test_storage();

        assert_eq!(storage.load_term().unwrap(), 0);

        storage.save_term(5).unwrap();
        assert_eq!(storage.load_term().unwrap(), 5);

        storage.save_term(100).unwrap();
        assert_eq!(storage.load_term().unwrap(), 100);
    }

    #[test]
    fn test_file_storage_voted_for() {
        let (mut storage, _dir) = test_storage();

        assert_eq!(storage.load_voted_for().unwrap(), None);

        storage.save_voted_for(Some(3)).unwrap();
        assert_eq!(storage.load_voted_for().unwrap(), Some(3));

        storage.save_voted_for(None).unwrap();
        assert_eq!(storage.load_voted_for().unwrap(), None);
    }

    #[test]
    fn test_file_storage_log() {
        let (mut storage, _dir) = test_storage();

        assert_eq!(storage.load_log().unwrap().len(), 0);

        let entries = vec![
            LogEntry { term: 1, index: 1, command: "SET x=1".to_string() },
            LogEntry { term: 1, index: 2, command: "SET y=2".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();

        let loaded = storage.load_log().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].command, "SET x=1");
        assert_eq!(loaded[1].command, "SET y=2");
    }

    #[test]
    fn test_file_storage_log_truncate() {
        let (mut storage, _dir) = test_storage();

        let entries = vec![
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
            LogEntry { term: 2, index: 3, command: "CMD 3".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();

        storage.truncate_log(2).unwrap();

        let loaded = storage.load_log().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].index, 1);
    }

    #[test]
    fn test_file_storage_log_compact() {
        let (mut storage, _dir) = test_storage();

        let entries = vec![
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
            LogEntry { term: 2, index: 3, command: "CMD 3".to_string() },
        ];
        storage.append_log_entries(&entries).unwrap();

        storage.compact_log(2).unwrap();

        let loaded = storage.load_log().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].index, 2);
        assert_eq!(loaded[1].index, 3);
    }

    #[test]
    fn test_file_storage_persistence_across_instances() {
        let dir = TempDir::new().unwrap();

        // First instance - write data
        {
            let mut storage = FileStorage::new(dir.path()).unwrap();
            storage.save_term(42).unwrap();
            storage.save_voted_for(Some(7)).unwrap();
            storage.append_log_entries(&[
                LogEntry { term: 42, index: 1, command: "HELLO".to_string() },
            ]).unwrap();
        }

        // Second instance - read data (simulates restart)
        {
            let storage = FileStorage::new(dir.path()).unwrap();
            assert_eq!(storage.load_term().unwrap(), 42);
            assert_eq!(storage.load_voted_for().unwrap(), Some(7));
            let log = storage.load_log().unwrap();
            assert_eq!(log.len(), 1);
            assert_eq!(log[0].command, "HELLO");
        }
    }

    #[test]
    fn test_detects_corrupted_term() {
        let dir = TempDir::new().unwrap();
        let mut storage = FileStorage::new(dir.path()).unwrap();

        storage.save_term(42).unwrap();

        // Corrupt the file by modifying data but not checksum
        let term_path = dir.path().join("term");
        fs::write(&term_path, "99 12345678\n").unwrap(); // wrong checksum

        let result = storage.load_term();
        assert!(matches!(result, Err(StorageError::Corruption(_))));
    }

    #[test]
    fn test_detects_corrupted_voted_for() {
        let dir = TempDir::new().unwrap();
        let mut storage = FileStorage::new(dir.path()).unwrap();

        storage.save_voted_for(Some(5)).unwrap();

        // Corrupt the file
        let path = dir.path().join("voted_for");
        fs::write(&path, "7 00000000\n").unwrap(); // wrong checksum

        let result = storage.load_voted_for();
        assert!(matches!(result, Err(StorageError::Corruption(_))));
    }

    #[test]
    fn test_detects_corrupted_log_entry() {
        let dir = TempDir::new().unwrap();
        let mut storage = FileStorage::new(dir.path()).unwrap();

        storage
            .append_log_entries(&[LogEntry {
                term: 1,
                index: 1,
                command: "CMD".to_string(),
            }])
            .unwrap();

        // Corrupt by appending a bad line
        let log_path = dir.path().join("log");
        let mut file = OpenOptions::new().append(true).open(&log_path).unwrap();
        writeln!(file, "{{\"term\":2,\"index\":2,\"command\":\"BAD\"}} deadbeef").unwrap();

        let result = storage.load_log();
        assert!(matches!(result, Err(StorageError::Corruption(_))));
    }

    #[test]
    fn test_crc32_basic() {
        // Test vector: "123456789" should have CRC32 = 0xCBF43926
        let data = b"123456789";
        assert_eq!(crc32(data), 0xCBF43926);
    }

    #[test]
    fn test_file_storage_snapshot() {
        use crate::core::snapshot::{Snapshot, SnapshotMetadata};

        let (mut storage, _dir) = test_storage();

        // Initially no snapshot
        assert!(storage.load_snapshot().unwrap().is_none());

        // Save a snapshot
        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                last_included_index: 10,
                last_included_term: 2,
            },
            data: vec![1, 2, 3, 4, 5],
        };
        storage.save_snapshot(&snapshot).unwrap();

        // Load it back
        let loaded = storage.load_snapshot().unwrap().unwrap();
        assert_eq!(loaded.metadata.last_included_index, 10);
        assert_eq!(loaded.metadata.last_included_term, 2);
        assert_eq!(loaded.data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_file_storage_snapshot_persistence() {
        use crate::core::snapshot::{Snapshot, SnapshotMetadata};

        let dir = TempDir::new().unwrap();

        // First instance - save snapshot
        {
            let mut storage = FileStorage::new(dir.path()).unwrap();
            let snapshot = Snapshot {
                metadata: SnapshotMetadata {
                    last_included_index: 100,
                    last_included_term: 5,
                },
                data: vec![10, 20, 30],
            };
            storage.save_snapshot(&snapshot).unwrap();
        }

        // Second instance - load snapshot (simulates restart)
        {
            let storage = FileStorage::new(dir.path()).unwrap();
            let loaded = storage.load_snapshot().unwrap().unwrap();
            assert_eq!(loaded.metadata.last_included_index, 100);
            assert_eq!(loaded.metadata.last_included_term, 5);
            assert_eq!(loaded.data, vec![10, 20, 30]);
        }
    }

    #[test]
    fn test_detects_corrupted_snapshot() {
        use crate::core::snapshot::{Snapshot, SnapshotMetadata};

        let dir = TempDir::new().unwrap();
        let mut storage = FileStorage::new(dir.path()).unwrap();

        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                last_included_index: 10,
                last_included_term: 2,
            },
            data: vec![1, 2, 3],
        };
        storage.save_snapshot(&snapshot).unwrap();

        // Corrupt the snapshot file
        let snapshot_path = dir.path().join("snapshot");
        fs::write(&snapshot_path, "{\"bad\":\"data\"} 12345678\n").unwrap();

        let result = storage.load_snapshot();
        assert!(matches!(result, Err(StorageError::Corruption(_))));
    }
}
