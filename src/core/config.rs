//! Raft configuration parameters

use std::time::Duration;

/// Configuration for Raft timing parameters
#[derive(Debug, Clone)]
pub struct RaftConfig {
    /// Interval between heartbeats sent by leader (default: 150ms)
    pub heartbeat_interval: Duration,
    /// Minimum election timeout (default: 300ms)
    pub election_timeout_min: Duration,
    /// Maximum election timeout (default: 500ms)
    pub election_timeout_max: Duration,
    /// Number of applied log entries before triggering automatic snapshot (default: 1000)
    /// Set to 0 to disable automatic snapshots
    pub snapshot_threshold: u64,
}

impl Default for RaftConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_millis(150),
            election_timeout_min: Duration::from_millis(300),
            election_timeout_max: Duration::from_millis(500),
            snapshot_threshold: 1000,
        }
    }
}

impl RaftConfig {
    /// Create a new config with custom heartbeat interval
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Create a new config with custom election timeout range
    pub fn with_election_timeout(mut self, min: Duration, max: Duration) -> Self {
        self.election_timeout_min = min;
        self.election_timeout_max = max;
        self
    }

    /// Create a new config with custom snapshot threshold
    /// Set to 0 to disable automatic snapshots
    pub fn with_snapshot_threshold(mut self, threshold: u64) -> Self {
        self.snapshot_threshold = threshold;
        self
    }

    /// Generate a random election timeout within the configured range
    pub fn random_election_timeout(&self) -> Duration {
        use rand::Rng;
        let min_ms = self.election_timeout_min.as_millis() as u64;
        let max_ms = self.election_timeout_max.as_millis() as u64;
        let timeout_ms = rand::rng().random_range(min_ms..=max_ms);
        Duration::from_millis(timeout_ms)
    }
}
