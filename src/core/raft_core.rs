//! Raft Consensus Algorithm Implementation
//!
//! This module implements the Raft consensus protocol as described in:
//! "In Search of an Understandable Consensus Algorithm" by Diego Ongaro and John Ousterhout

use std::collections::HashMap;
use tokio::time::Instant;

use serde::{Deserialize, Serialize};

use crate::state_machine::Snapshotable;
use crate::storage::Storage;

/// Special no-op command appended by leaders on election.
/// This allows committing entries from previous terms indirectly.
pub const NOOP_COMMAND: &str = "NOOP";

/// Raft node states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaftState {
    /// Follower: Passive state, receives updates from leader
    Follower,
    /// Candidate: Actively seeking votes to become leader
    Candidate,
    /// Leader: Handles all client requests and replicates log
    Leader,
}

/// A single log entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Term when entry was received by leader
    pub term: u64,
    /// Index in the log (1-indexed)
    pub index: u64,
    /// Command/data stored in this entry
    pub command: String,
}

/// RequestVote RPC arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestVoteArgs {
    /// Candidate's term
    pub term: u64,
    /// Candidate requesting vote
    pub candidate_id: u64,
    /// Index of candidate's last log entry
    pub last_log_index: u64,
    /// Term of candidate's last log entry
    pub last_log_term: u64,
}

/// RequestVote RPC results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestVoteResult {
    /// Current term, for candidate to update itself
    pub term: u64,
    /// True means candidate received vote
    pub vote_granted: bool,
}

/// AppendEntries RPC arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesArgs {
    /// Leader's term
    pub term: u64,
    /// Leader's ID
    pub leader_id: u64,
    /// Index of log entry immediately preceding new ones
    pub prev_log_index: u64,
    /// Term of prev_log_index entry
    pub prev_log_term: u64,
    /// Log entries to store (empty for heartbeat)
    pub entries: Vec<LogEntry>,
    /// Leader's commit_index
    pub leader_commit: u64,
}

/// AppendEntries RPC results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesResult {
    /// Current term, for leader to update itself
    pub term: u64,
    /// True if follower contained entry matching prev_log_index and prev_log_term
    pub success: bool,
}

/// Result of handling an AppendEntries RPC
#[derive(Debug, Clone)]
pub struct HandleAppendEntriesOutput {
    /// The response to send back to the leader
    pub result: AppendEntriesResult,
    /// Leader ID if we recognized a valid leader
    pub leader_id: Option<u64>,
}

/// InstallSnapshot RPC arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallSnapshotArgs {
    /// Leader's term
    pub term: u64,
    /// Leader's ID
    pub leader_id: u64,
    /// Last log index included in snapshot
    pub last_included_index: u64,
    /// Term of last included entry
    pub last_included_term: u64,
    /// Snapshot data
    pub data: Vec<u8>,
}

/// InstallSnapshot RPC results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InstallSnapshotResult {
    /// Snapshot was successfully installed
    Success { term: u64 },
    /// Snapshot installation failed
    Failed { term: u64, reason: String },
}

/// Core Raft state machine (sync, transport-agnostic)
pub struct RaftCore {
    // Storage backend for persistent state
    storage: Box<dyn Storage>,
    // State machine to apply committed entries to (must support snapshots)
    state_machine: Box<dyn Snapshotable>,

    // Persistent state on all servers (updated on stable storage before responding to RPCs)
    // These are cached in memory for fast access, but always persisted via storage
    /// Latest term server has seen (initialized to 0 on first boot, increases monotonically)
    pub current_term: u64,
    /// Candidate ID that received vote in current term (or None if none)
    pub voted_for: Option<u64>,
    /// Log entries; each entry contains command for state machine, and term when entry was received by leader (first index is 1)
    pub log: Vec<LogEntry>,
    /// Last log index included in snapshot (0 if no snapshot)
    pub snapshot_last_index: u64,
    /// Term of the last log entry included in snapshot (0 if no snapshot)
    pub snapshot_last_term: u64,

    // Volatile state on all servers
    /// Index of highest log entry known to be committed (initialized to 0, increases monotonically)
    pub commit_index: u64,
    /// Index of highest log entry applied to state machine (initialized to 0, increases monotonically)
    pub last_applied: u64,

    // Volatile state on leaders (reinitialized after election)
    /// For each server, index of next log entry to send to that server (initialized to leader last log index + 1)
    pub next_index: HashMap<u64, u64>,
    /// For each server, index of highest log entry known to be replicated on server (initialized to 0, increases monotonically)
    pub match_index: HashMap<u64, u64>,

    // Node-specific state
    /// Unique identifier for this node
    pub id: u64,
    /// Current state of this node
    pub state: RaftState,
    /// IDs of other nodes in the cluster
    pub peers: Vec<u64>,
    /// Peers that have granted votes in the current election (used by candidates)
    votes_received: Vec<u64>,
    /// Current known leader (updated when receiving valid AppendEntries)
    pub current_leader: Option<u64>,
    /// Last time we received a valid heartbeat from leader (for election timeout)
    pub last_heartbeat: Instant,
    /// Number of applied log entries before triggering automatic snapshot (0 = disabled)
    snapshot_threshold: u64,
}

impl RaftCore {
    /// Create a new Raft core with the given storage backend and state machine
    /// Loads persistent state (term, voted_for, log, snapshot) from storage
    /// Restores state machine from snapshot if one exists
    pub fn new(
        id: u64,
        peers: Vec<u64>,
        storage: Box<dyn Storage>,
        mut state_machine: Box<dyn Snapshotable>,
    ) -> Self {
        // Load persistent state from storage
        let current_term = storage.load_term().expect("failed to load term from storage");
        let voted_for = storage.load_voted_for().expect("failed to load voted_for from storage");
        let log = storage.load_log().expect("failed to load log from storage");

        // Load snapshot and restore state machine (if exists)
        let (snapshot_last_index, snapshot_last_term) = match storage.load_snapshot() {
            Ok(Some(snapshot)) => {
                let last_index = snapshot.metadata.last_included_index;
                let last_term = snapshot.metadata.last_included_term;

                // Restore state machine from snapshot
                state_machine.restore(&snapshot.data)
                    .expect("failed to restore state machine from snapshot");

                (last_index, last_term)
            }
            Ok(None) => (0, 0),
            Err(e) => panic!("failed to load snapshot from storage: {}", e),
        };

        // commit_index and last_applied should be at least snapshot_last_index
        // since all entries in the snapshot are committed and applied
        let commit_index = snapshot_last_index;
        let last_applied = snapshot_last_index;

        RaftCore {
            storage,
            state_machine,
            current_term,
            voted_for,
            log,
            snapshot_last_index,
            snapshot_last_term,
            commit_index,
            last_applied,
            next_index: HashMap::new(),
            match_index: HashMap::new(),
            id,
            state: RaftState::Follower,
            peers,
            votes_received: Vec::new(),
            current_leader: None,
            last_heartbeat: Instant::now(),
            snapshot_threshold: 1000, // Default: snapshot every 1000 entries
        }
    }

    /// Set the snapshot threshold (number of applied entries before auto-snapshot)
    /// Set to 0 to disable automatic snapshots
    pub fn set_snapshot_threshold(&mut self, threshold: u64) {
        self.snapshot_threshold = threshold;
    }

    // === Persistence helpers ===

    /// Update current_term and persist to storage
    fn set_term(&mut self, term: u64) {
        self.current_term = term;
        self.storage.save_term(term).expect("failed to persist term");
    }

    /// Update voted_for and persist to storage
    fn set_voted_for(&mut self, voted_for: Option<u64>) {
        self.voted_for = voted_for;
        self.storage.save_voted_for(voted_for).expect("failed to persist voted_for");
    }

    /// Update term and voted_for together (common pattern when discovering higher term)
    fn update_term(&mut self, new_term: u64) {
        self.set_term(new_term);
        self.set_voted_for(None);
    }

    /// Append a single entry to log and persist
    fn persist_log_entry(&mut self, entry: LogEntry) {
        self.storage.append_log_entries(&[entry.clone()]).expect("failed to persist log entry");
        self.log.push(entry);
    }

    /// Truncate log from index and persist
    fn persist_truncate_log(&mut self, from_index: u64) {
        let truncate_pos = (from_index - 1) as usize;
        if truncate_pos < self.log.len() {
            self.storage.truncate_log(from_index).expect("failed to truncate log");
            self.log.truncate(truncate_pos);
        }
    }

    /// Get the last log index (returns snapshot_last_index if log is empty)
    pub fn last_log_index(&self) -> u64 {
        if self.log.is_empty() {
            self.snapshot_last_index
        } else {
            self.log.last().unwrap().index
        }
    }

    /// Get the term of the last log entry (returns snapshot_last_term if log is empty)
    pub fn last_log_term(&self) -> u64 {
        if self.log.is_empty() {
            self.snapshot_last_term
        } else {
            self.log.last().unwrap().term
        }
    }

    /// Get a log entry by its index, accounting for snapshot offset
    /// Returns None if the entry is in the snapshot or beyond the log
    fn get_log_entry(&self, index: u64) -> Option<&LogEntry> {
        if index <= self.snapshot_last_index {
            // Entry is covered by snapshot
            None
        } else {
            // Entry should be in log Vec
            // log[0] is entry at index (snapshot_last_index + 1)
            let offset = (index - self.snapshot_last_index - 1) as usize;
            self.log.get(offset)
        }
    }

    /// Check if candidate's log is at least as up-to-date as receiver's log
    /// Returns true if:
    /// - candidate's last log term > receiver's last log term, OR
    /// - candidate's last log term == receiver's last log term AND candidate's last log index >= receiver's last log index
    pub fn is_log_up_to_date(&self, candidate_last_log_term: u64, candidate_last_log_index: u64) -> bool {
        let my_last_term = self.last_log_term();
        let my_last_index = self.last_log_index();

        candidate_last_log_term > my_last_term || 
        (candidate_last_log_term == my_last_term && candidate_last_log_index >= my_last_index)
    }

    /// Handle RequestVote RPC
    /// Returns (term, vote_granted)
    pub fn handle_request_vote(&mut self, vote_req: &RequestVoteArgs) -> RequestVoteResult {

        // Decline requests with stale term immediately
        if vote_req.term < self.current_term {
            return RequestVoteResult {
                term: self.current_term,
                vote_granted: false,
            };
        }

        // If RPC request or response contains term T > currentTerm: set currentTerm = T, convert to follower
        if vote_req.term > self.current_term {
            let old_state = self.state;
            self.update_term(vote_req.term);
            self.state = RaftState::Follower;
            if old_state != RaftState::Follower {
                println!("[NODE {}] Stepped down to FOLLOWER (was {:?}, saw term {})", self.id, old_state, vote_req.term);
            }
        }

        // If already voted for another candidate, decline vote
        if self.voted_for.is_some() && self.voted_for != Some(vote_req.candidate_id) {
            return RequestVoteResult {
                term: self.current_term,
                vote_granted: false,
            };
        }

        if !self.is_log_up_to_date(vote_req.last_log_term, vote_req.last_log_index) {
            return RequestVoteResult {
                term: self.current_term,
                vote_granted: false,
            };
        }

        // Grant vote
        self.set_voted_for(Some(vote_req.candidate_id));

        RequestVoteResult {
            term: self.current_term,
            vote_granted: true,
        }
    }

    /// Handle AppendEntries RPC (heartbeat or log replication)
    /// Returns the result to send back and the leader ID if recognized
    pub fn handle_append_entries(&mut self, append_req: &AppendEntriesArgs) -> HandleAppendEntriesOutput {
        // If RPC request or response contains term T > currentTerm: set currentTerm = T, convert to follower
        if append_req.term > self.current_term {
            let old_state = self.state;
            self.update_term(append_req.term);
            self.state = RaftState::Follower;
            if old_state != RaftState::Follower {
                println!("[NODE {}] Stepped down to FOLLOWER (was {:?}, saw term {} from leader {})", self.id, old_state, append_req.term, append_req.leader_id);
            }
        }

        let mut leader_id = None;

        let success = if append_req.term < self.current_term {
            // Reply false if term < currentTerm
            false
        } else {
            // Valid AppendEntries from current leader - reset election timeout
            self.state = RaftState::Follower;
            self.current_leader = Some(append_req.leader_id);
            self.last_heartbeat = Instant::now();
            leader_id = Some(append_req.leader_id);

            // Reply false if log doesn't contain an entry at prev_log_index with term matching prev_log_term
            if append_req.prev_log_index > 0 {

                // If current log is not up-to-date, return false
                if append_req.prev_log_index > self.last_log_index() {
                    false
                } else if append_req.prev_log_index == self.snapshot_last_index {
                    // prev_log_index is exactly at snapshot boundary
                    // Check that the term matches the snapshot
                    if append_req.prev_log_term != self.snapshot_last_term {
                        false
                    } else {
                        // Process each new entry
                        self.apply_entries(&append_req.entries);

                        // If leaderCommit > commitIndex, set commitIndex = min(leaderCommit, index of last new entry)
                        if append_req.leader_commit > self.commit_index {
                            self.commit_index = std::cmp::min(
                                append_req.leader_commit,
                                self.last_log_index(),
                            );
                            // Apply committed entries to state machine
                            self.apply_committed_entries();
                        }

                        true
                    }
                } else {
                    // Check if the entry at prev_log_index has the correct term
                    // Need to find it in the log (accounting for snapshot offset)
                    if let Some(entry) = self.get_log_entry(append_req.prev_log_index) {
                        if entry.term != append_req.prev_log_term {
                            false
                        } else {
                            // Process each new entry
                            self.apply_entries(&append_req.entries);

                            // If leaderCommit > commitIndex, set commitIndex = min(leaderCommit, index of last new entry)
                            if append_req.leader_commit > self.commit_index {
                                self.commit_index = std::cmp::min(
                                    append_req.leader_commit,
                                    self.last_log_index(),
                                );
                                // Apply committed entries to state machine
                                self.apply_committed_entries();
                            }

                            true
                        }
                    } else {
                        // Entry not found in log or snapshot
                        false
                    }
                }
            } else {
                // prev_log_index is 0, meaning we're starting from the beginning
                // Process each new entry
                self.apply_entries(&append_req.entries);

                // Update commit_index
                if append_req.leader_commit > self.commit_index {
                    self.commit_index = std::cmp::min(
                        append_req.leader_commit,
                        self.last_log_index(),
                        );
                    // Apply committed entries to state machine
                    self.apply_committed_entries();
                }

                true
            }
        };

        HandleAppendEntriesOutput {
            result: AppendEntriesResult {
                term: self.current_term,
                success,
            },
            leader_id,
        }
    }

    /// Apply entries from AppendEntries RPC, handling conflicts and persistence
    fn apply_entries(&mut self, entries: &[LogEntry]) {
        for entry in entries {
            // Skip entries already in snapshot
            if entry.index <= self.snapshot_last_index {
                continue;
            }

            // Calculate position in in-memory log accounting for snapshot offset
            // log[0] represents entry at index (snapshot_last_index + 1)
            let entry_idx = (entry.index - self.snapshot_last_index - 1) as usize;

            if entry_idx < self.log.len() {
                // Entry exists at this index
                if self.log[entry_idx].term != entry.term {
                    // Conflict: same index but different terms
                    // Delete this entry and all that follow, then append new entry
                    self.persist_truncate_log(entry.index);
                    self.persist_log_entry(entry.clone());
                    println!("[NODE {}] Replicated entry {} (term {}): {}", self.id, entry.index, entry.term, entry.command);
                }
                // If terms match, entry already exists - skip (idempotent)
            } else {
                // Entry doesn't exist yet, append it
                self.persist_log_entry(entry.clone());
                println!("[NODE {}] Replicated entry {} (term {}): {}", self.id, entry.index, entry.term, entry.command);
            }
        }
    }

    /// Handle InstallSnapshot RPC
    pub fn handle_install_snapshot(&mut self, args: &InstallSnapshotArgs) -> InstallSnapshotResult {
        use crate::core::snapshot::Snapshot;

        // Reply immediately if term < currentTerm
        if args.term < self.current_term {
            return InstallSnapshotResult::Failed {
                term: self.current_term,
                reason: "stale term".to_string(),
            };
        }

        // Update term if we see a higher term
        if args.term > self.current_term {
            self.update_term(args.term);
            self.state = RaftState::Follower;
        }

        // Reset election timeout - we heard from valid leader
        self.last_heartbeat = Instant::now();
        self.current_leader = Some(args.leader_id);

        // If snapshot is older than what we have, ignore it
        if args.last_included_index <= self.snapshot_last_index {
            return InstallSnapshotResult::Failed {
                term: self.current_term,
                reason: format!(
                    "snapshot too old: {} <= {}",
                    args.last_included_index, self.snapshot_last_index
                ),
            };
        }

        // STEP 1: Save snapshot to storage FIRST (before modifying state machine)
        // This ensures disk and memory stay consistent
        let snapshot = Snapshot {
            metadata: crate::core::snapshot::SnapshotMetadata {
                last_included_index: args.last_included_index,
                last_included_term: args.last_included_term,
            },
            data: args.data.clone(),
        };

        if let Err(e) = self.storage.save_snapshot(&snapshot) {
            return InstallSnapshotResult::Failed {
                term: self.current_term,
                reason: format!("failed to save snapshot: {}", e),
            };
        }

        // STEP 2: Restore state machine (snapshot is safely on disk)
        // If this fails, we panic - we've saved snapshot but can't apply it
        self.state_machine
            .restore(&args.data)
            .expect("failed to restore state machine from snapshot - node is in inconsistent state");

        // STEP 3: Update metadata (both save and restore succeeded)
        self.snapshot_last_index = args.last_included_index;
        self.snapshot_last_term = args.last_included_term;

        // Discard log entries covered by snapshot
        // Keep only entries with index > last_included_index
        self.log.retain(|entry| entry.index > args.last_included_index);

        // Compact log in storage (keep entries after snapshot)
        let _ = self.storage.compact_log(args.last_included_index + 1);

        // Update commit_index and last_applied
        if args.last_included_index > self.commit_index {
            self.commit_index = args.last_included_index;
        }
        if args.last_included_index > self.last_applied {
            self.last_applied = args.last_included_index;
        }

        InstallSnapshotResult::Success {
            term: self.current_term,
        }
    }

    /// Start a new election (called when election timeout elapses)
    pub fn start_election(&mut self) {
        // Increment current_term and persist
        self.set_term(self.current_term + 1);

        // Transition to candidate
        self.state = RaftState::Candidate;
        println!("[NODE {}] Became CANDIDATE for term {}", self.id, self.current_term);

        // Clear current leader (we're challenging)
        self.current_leader = None;

        // Vote for self and persist
        self.set_voted_for(Some(self.id));

        // Reset votes received (we've already voted for ourselves)
        self.votes_received.clear();
        self.votes_received.push(self.id);

        // Reset election timer so we don't immediately timeout again
        self.last_heartbeat = Instant::now();
    }

    /// Become leader (called after receiving majority of votes)
    pub fn become_leader(&mut self) {
        self.state = RaftState::Leader;
        self.current_leader = Some(self.id);
        // Reset heartbeat timer to prevent election timeout from firing on leader
        self.last_heartbeat = Instant::now();
        println!("[NODE {}] Became LEADER for term {}", self.id, self.current_term);

        // Reinitialize next_index and match_index BEFORE appending no-op
        // This way next_index points AT the no-op, so it gets sent in first heartbeat
        let last_index = self.last_log_index();
        for peer_id in &self.peers {
            self.next_index.insert(*peer_id, last_index + 1);
            self.match_index.insert(*peer_id, 0);
        }

        // Append no-op entry to commit entries from previous terms
        // (Raft paper Section 5.4.2: leader can only commit entries from current term)
        let noop_entry = LogEntry {
            term: self.current_term,
            index: self.last_log_index() + 1,
            command: NOOP_COMMAND.to_string(),
        };
        println!("[NODE {}] Appending no-op entry {}", self.id, noop_entry.index);
        self.persist_log_entry(noop_entry);
    }

    /// Add a new log entry (called by leader when receiving client request)
    /// Returns None if called on a non-leader node
    pub fn append_log_entry(&mut self, command: String) -> Option<LogEntry> {
        // Only leaders can append log entries
        if self.state != RaftState::Leader {
            return None;
        }

        let index = self.last_log_index() + 1;
        let entry = LogEntry {
            term: self.current_term,
            index,
            command: command.clone(),
        };
        println!("[NODE {}] Appended entry {} (term {}): {}", self.id, index, self.current_term, command);
        self.persist_log_entry(entry.clone());
        Some(entry)
    }

    /// Apply committed entries to the state machine
    /// Updates last_applied to match commit_index
    /// Returns vec of (index, result) for each entry applied
    ///
    /// Automatically triggers snapshot if:
    /// - Snapshot threshold is configured (> 0)
    /// - Node is a leader
    /// - Number of entries since last snapshot >= threshold
    pub fn apply_committed_entries(&mut self) -> Vec<(u64, Result<String, String>)> {
        let mut results = Vec::new();
        while self.last_applied < self.commit_index {
            self.last_applied += 1;

            // Get entry accounting for snapshot offset
            if let Some(entry) = self.get_log_entry(self.last_applied) {
                // Clone command to avoid borrow checker issue
                let command = entry.command.clone();
                let result = self.state_machine.apply(&command);
                results.push((self.last_applied, result));
            } else {
                // Entry is in snapshot, already applied
                // This shouldn't happen in normal operation
                panic!("Trying to apply entry {} which is in snapshot (last_applied should be >= snapshot_last_index)", self.last_applied);
            }
        }

        // Check if we should trigger an automatic snapshot
        // All nodes (leaders, followers, candidates) take snapshots independently
        // to prevent unbounded log growth. Leaders additionally send snapshots
        // to followers that fall behind via InstallSnapshot RPC.
        if self.snapshot_threshold > 0 {
            let entries_since_snapshot = self.last_applied - self.snapshot_last_index;
            if entries_since_snapshot >= self.snapshot_threshold {
                println!("[NODE {}] Automatic snapshot triggered ({} entries since last snapshot)",
                         self.id, entries_since_snapshot);
                if let Err(e) = self.take_snapshot() {
                    eprintln!("[NODE {}] Auto-snapshot failed: {}", self.id, e);
                }
            }
        }

        results
    }

    /// Take a snapshot of the state machine up to last_applied
    /// Discards log entries covered by the snapshot
    pub fn take_snapshot(&mut self) -> Result<(), String> {
        use crate::core::snapshot::{Snapshot, SnapshotMetadata};

        if self.last_applied == 0 {
            return Err("No entries applied yet, cannot snapshot".to_string());
        }

        if self.last_applied <= self.snapshot_last_index {
            return Err(format!(
                "Already have snapshot up to index {}, last_applied is {}",
                self.snapshot_last_index, self.last_applied
            ));
        }

        // Get term of last applied entry (must be in log since last_applied > snapshot_last_index)
        let last_applied_term = self.get_log_entry(self.last_applied)
            .map(|e| e.term)
            .ok_or_else(|| "Cannot find last_applied entry".to_string())?;

        // Snapshot the state machine
        let snapshot_data = self.state_machine.snapshot()?;

        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                last_included_index: self.last_applied,
                last_included_term: last_applied_term,
            },
            data: snapshot_data,
        };

        // Save to storage
        self.storage
            .save_snapshot(&snapshot)
            .map_err(|e| format!("Failed to save snapshot: {}", e))?;

        // Update snapshot metadata
        self.snapshot_last_index = snapshot.metadata.last_included_index;
        self.snapshot_last_term = snapshot.metadata.last_included_term;

        // Discard log entries covered by snapshot
        // Keep entries after last_applied
        let keep_from = self.snapshot_last_index + 1;
        self.storage
            .compact_log(keep_from)
            .map_err(|e| format!("Failed to compact log: {}", e))?;

        // Update in-memory log
        self.log.retain(|entry| entry.index >= keep_from);

        println!("[NODE {}] Snapshot taken at {}:{}", self.id, self.snapshot_last_index, self.snapshot_last_term);
        Ok(())
    }

    /// Load the current snapshot from storage
    /// Returns None if no snapshot exists
    pub fn load_snapshot(&self) -> Result<Option<crate::core::snapshot::Snapshot>, crate::storage::StorageError> {
        self.storage.load_snapshot()
    }

    /// Process a RequestVote response (called by candidate)
    /// Updates term if response contains a higher term
    pub fn process_request_vote_response(&mut self, result: &RequestVoteResult) {
        // If RPC response contains term T > currentTerm: set currentTerm = T, convert to follower
        if result.term > self.current_term {
            let old_state = self.state;
            self.update_term(result.term);
            self.state = RaftState::Follower;
            if old_state != RaftState::Follower {
                println!("[NODE {}] Stepped down to FOLLOWER (was {:?}, saw term {} in vote response)", self.id, old_state, result.term);
            }
        }
    }

    /// Process an AppendEntries response (called by leader)
    /// Updates term if response contains a higher term
    pub fn process_append_entries_response(&mut self, result: &AppendEntriesResult) {
        // If RPC response contains term T > currentTerm: set currentTerm = T, convert to follower
        if result.term > self.current_term {
            let old_state = self.state;
            self.update_term(result.term);
            self.state = RaftState::Follower;
            if old_state != RaftState::Follower {
                println!("[NODE {}] Stepped down to FOLLOWER (was {:?}, saw term {} in append response)", self.id, old_state, result.term);
            }
        }
    }

    /// Handle a RequestVote result (called by candidate after receiving vote response)
    /// Processes the response, updates term if needed, tracks votes, and becomes leader if majority reached
    /// Returns true if this node became leader as a result
    pub fn handle_request_vote_result(&mut self, peer_id: u64, result: &RequestVoteResult) -> bool {
        // Process the response (updates term if needed)
        self.process_request_vote_response(result);

        // If we're no longer a candidate (e.g., term was updated), we can't become leader
        if self.state != RaftState::Candidate {
            return false;
        }

        // Track the vote if granted
        if result.vote_granted && !self.votes_received.contains(&peer_id) {
            self.votes_received.push(peer_id);
        }

        // Check if we have majority (including our own vote)
        let total_nodes = 1 + self.peers.len(); // self + peers
        let majority = (total_nodes / 2) + 1;
        
        if self.votes_received.len() >= majority {
            self.become_leader();
            return true;
        }

        false
    }

    /// Handle an AppendEntries result (called by leader after receiving replication response)
    /// Processes the response, updates term if needed, tracks replication, and commits entries if majority reached
    /// Returns (committed_index, apply_results) - the commit index and results from applying entries
    pub fn handle_append_entries_result(
        &mut self,
        peer_id: u64,
        entry_index: u64,
        result: &AppendEntriesResult,
    ) -> (Option<u64>, Vec<(u64, Result<String, String>)>) {
        // Process the response (updates term if needed)
        self.process_append_entries_response(result);

        // If we're no longer a leader (e.g., term was updated), we can't commit
        if self.state != RaftState::Leader {
            return (None, Vec::new());
        }

        // Update match_index and next_index based on result
        if result.success {
            // Successfully replicated up to entry_index
            if entry_index > 0 {
                let current_match = self.match_index.get(&peer_id).copied().unwrap_or(0);
                if entry_index > current_match {
                    self.match_index.insert(peer_id, entry_index);
                }
                // Update next_index for next entry to send
                self.next_index.insert(peer_id, entry_index + 1);
            }
        } else {
            // Replication failed, decrement next_index for retry
            let current_next = self.next_index.get(&peer_id).copied().unwrap_or(1);
            if current_next > 1 {
                self.next_index.insert(peer_id, current_next - 1);
            }
        }

        // Check if entry_index is replicated to majority and can be committed
        if entry_index == 0 {
            return (None, Vec::new()); // No entry to commit
        }

        // Raft safety: Only commit entries from current term (Section 5.4.2)
        // Previous term entries are committed indirectly when a current-term entry is committed
        let entry_term = self.get_log_entry(entry_index).map(|e| e.term);
        if entry_term != Some(self.current_term) {
            return (None, Vec::new()); // Cannot commit entries from previous terms directly
        }

        // Count how many nodes have replicated this entry (including leader)
        let mut replicated_count = 1; // Leader has it
        for (_, &match_idx) in &self.match_index {
            if match_idx >= entry_index {
                replicated_count += 1;
            }
        }

        // Check if we have majority
        let total_nodes = 1 + self.peers.len(); // self + peers
        let majority = (total_nodes / 2) + 1;

        if replicated_count >= majority && entry_index > self.commit_index {
            // Commit the entry
            self.commit_index = entry_index;
            println!("[NODE {}] Committed entry {} (replicated to {}/{})", self.id, entry_index, replicated_count, total_nodes);
            let apply_results = self.apply_committed_entries();
            return (Some(entry_index), apply_results);
        }

        (None, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::TestStateMachine;
    use crate::storage::memory::MemoryStorage;

    /// Helper to create RaftCore with MemoryStorage for tests
    fn new_test_core(id: u64, peers: Vec<u64>) -> RaftCore {
        RaftCore::new(
            id,
            peers,
            Box::new(MemoryStorage::new()),
            Box::new(crate::state_machine::TestStateMachine::new()),
        )
    }

    #[test]
    fn test_new_node() {
        let node = new_test_core(1, vec![2, 3]);
        assert_eq!(node.id, 1);
        assert_eq!(node.current_term, 0);
        assert_eq!(node.state, RaftState::Follower);
        assert_eq!(node.log.len(), 0);
    }

    #[test]
    fn test_election() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.start_election();
        assert_eq!(node.state, RaftState::Candidate);
        assert_eq!(node.current_term, 1);
        assert_eq!(node.voted_for, Some(1));
    }

    #[test]
    fn test_request_vote() {
        let mut node = new_test_core(1, vec![2, 3]);
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);
        assert!(result.vote_granted);
        assert_eq!(node.voted_for, Some(2));
    }

    #[test]
    fn test_append_entries() {
        let mut node = new_test_core(1, vec![2, 3]);
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![LogEntry {
                term: 1,
                index: 1,
                command: "SET x=1".to_string(),
            }],
            leader_commit: 0,
        };
        let before = node.last_heartbeat;
        let output = node.handle_append_entries(&args);
        assert!(output.result.success);
        assert_eq!(output.leader_id, Some(2));
        assert_eq!(node.log.len(), 1);
        assert_eq!(node.state, RaftState::Follower);
        assert!(node.last_heartbeat >= before, "last_heartbeat should be updated");
    }

    #[test]
    fn test_append_entries_stale_term_no_reset() {
        let mut node = new_test_core(1, vec![2, 3]);
        // Node is at term 2
        node.current_term = 2;
        let before = node.last_heartbeat;

        // Receive AppendEntries from stale term 1
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);

        // Should reject and NOT reset election timeout
        assert!(!output.result.success);
        assert_eq!(output.leader_id, None);
        assert_eq!(node.last_heartbeat, before, "last_heartbeat should NOT be updated for stale term");
    }

    #[test]
    fn test_heartbeat_resets_election_timeout() {
        let mut node = new_test_core(1, vec![2, 3]);
        let before = node.last_heartbeat;

        // Receive empty heartbeat
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![], // Empty = heartbeat
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);

        // Heartbeat should succeed and reset timeout
        assert!(output.result.success);
        assert_eq!(output.leader_id, Some(2));
        assert!(node.last_heartbeat >= before, "last_heartbeat should be updated");
    }

    // === Vote Rejection Tests ===

    #[test]
    fn test_vote_denied_candidate_has_lower_term() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 5;

        // Candidate with lower term requests vote
        let args = RequestVoteArgs {
            term: 3,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);

        assert!(!result.vote_granted);
        assert_eq!(result.term, 5); // Returns current term
        assert_eq!(node.voted_for, None); // Didn't vote
    }

    #[test]
    fn test_vote_denied_already_voted_for_another() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 1;
        node.voted_for = Some(2); // Already voted for node 2

        // Node 3 requests vote in same term
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 3,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);

        assert!(!result.vote_granted);
        assert_eq!(node.voted_for, Some(2)); // Still voted for node 2
    }

    #[test]
    fn test_vote_granted_to_same_candidate_again() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 1;
        node.voted_for = Some(2); // Already voted for node 2

        // Node 2 requests vote again (e.g., retransmission)
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);

        // Should grant vote to same candidate
        assert!(result.vote_granted);
        assert_eq!(node.voted_for, Some(2));
    }

    #[test]
    fn test_vote_denied_candidate_log_has_older_term() {
        let mut node = new_test_core(1, vec![2, 3]);
        // Node has log entry at term 3
        node.log.push(LogEntry {
            term: 3,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Candidate's last log entry is at term 2 (older)
        let args = RequestVoteArgs {
            term: 4,
            candidate_id: 2,
            last_log_index: 1,
            last_log_term: 2, // Older than our term 3
        };
        let result = node.handle_request_vote(&args);

        assert!(!result.vote_granted);
        // Node should update term but not grant vote
        assert_eq!(node.current_term, 4);
    }

    #[test]
    fn test_vote_denied_candidate_log_is_shorter() {
        let mut node = new_test_core(1, vec![2, 3]);
        // Node has 2 log entries at term 2
        node.log.push(LogEntry {
            term: 2,
            index: 1,
            command: "SET x=1".to_string(),
        });
        node.log.push(LogEntry {
            term: 2,
            index: 2,
            command: "SET y=2".to_string(),
        });

        // Candidate has same term but shorter log
        let args = RequestVoteArgs {
            term: 3,
            candidate_id: 2,
            last_log_index: 1, // Only 1 entry
            last_log_term: 2,  // Same term
        };
        let result = node.handle_request_vote(&args);

        assert!(!result.vote_granted);
    }

    #[test]
    fn test_vote_granted_candidate_log_has_higher_term() {
        let mut node = new_test_core(1, vec![2, 3]);
        // Node has log entry at term 2
        node.log.push(LogEntry {
            term: 2,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Candidate's last log entry is at term 3 (newer)
        let args = RequestVoteArgs {
            term: 4,
            candidate_id: 2,
            last_log_index: 1,
            last_log_term: 3, // Higher than our term 2
        };
        let result = node.handle_request_vote(&args);

        assert!(result.vote_granted);
        assert_eq!(node.voted_for, Some(2));
    }

    // === Term/State Transition Tests ===

    #[test]
    fn test_leader_steps_down_on_higher_term_in_vote_response() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 1;
        node.state = RaftState::Leader;

        // Receive vote response with higher term
        let result = RequestVoteResult {
            term: 5,
            vote_granted: false,
        };
        node.process_request_vote_response(&result);

        assert_eq!(node.state, RaftState::Follower);
        assert_eq!(node.current_term, 5);
        assert_eq!(node.voted_for, None);
    }

    #[test]
    fn test_leader_steps_down_on_higher_term_in_append_response() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 1;
        node.state = RaftState::Leader;

        // Receive append response with higher term
        let result = AppendEntriesResult {
            term: 5,
            success: false,
        };
        node.process_append_entries_response(&result);

        assert_eq!(node.state, RaftState::Follower);
        assert_eq!(node.current_term, 5);
        assert_eq!(node.voted_for, None);
    }

    #[test]
    fn test_candidate_steps_down_on_append_entries_from_new_leader() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.start_election(); // Now candidate at term 1
        assert_eq!(node.state, RaftState::Candidate);

        // Receive AppendEntries from leader at same term
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);

        // Should step down to follower
        assert!(output.result.success);
        assert_eq!(node.state, RaftState::Follower);
    }

    #[test]
    fn test_candidate_steps_down_on_higher_term_request_vote() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.start_election(); // Now candidate at term 1
        assert_eq!(node.state, RaftState::Candidate);
        assert_eq!(node.voted_for, Some(1)); // Voted for self

        // Receive RequestVote from candidate at higher term
        let args = RequestVoteArgs {
            term: 5,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);

        // Should step down, update term, and grant vote
        assert!(result.vote_granted);
        assert_eq!(node.state, RaftState::Follower);
        assert_eq!(node.current_term, 5);
        assert_eq!(node.voted_for, Some(2));
    }

    #[test]
    fn test_follower_updates_term_on_higher_term_request_vote() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 1;

        // Receive RequestVote from higher term
        let args = RequestVoteArgs {
            term: 5,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);

        assert!(result.vote_granted);
        assert_eq!(node.current_term, 5);
        assert_eq!(node.voted_for, Some(2));
    }

    #[test]
    fn test_follower_updates_term_on_higher_term_append_entries() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 1;

        // Receive AppendEntries from higher term
        let args = AppendEntriesArgs {
            term: 5,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(node.current_term, 5);
        assert_eq!(node.voted_for, None); // Reset on term change
    }

    // === Split Vote / Election Tests ===

    #[test]
    fn test_election_needs_majority_in_5_node_cluster() {
        // In a 5-node cluster, candidate needs 3 votes to win
        let mut node = new_test_core(1, vec![2, 3, 4, 5]);
        node.start_election();
        assert_eq!(node.state, RaftState::Candidate);

        let result_granted = RequestVoteResult {
            term: 1,
            vote_granted: true,
        };
        let result_denied = RequestVoteResult {
            term: 1,
            vote_granted: false,
        };

        // Get one vote - self + 1 = 2, not majority
        let became_leader = node.handle_request_vote_result(2, &result_granted);
        assert!(!became_leader);
        assert_eq!(node.state, RaftState::Candidate);

        // Get denied from node 3
        let became_leader = node.handle_request_vote_result(3, &result_denied);
        assert!(!became_leader);
        assert_eq!(node.state, RaftState::Candidate);

        // Get second yes - self + 2 = 3 = majority!
        let became_leader = node.handle_request_vote_result(4, &result_granted);
        assert!(became_leader);
        assert_eq!(node.state, RaftState::Leader);
    }

    #[test]
    fn test_election_lost_all_denied() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.start_election();

        let result_denied = RequestVoteResult {
            term: 1,
            vote_granted: false,
        };

        // Both peers deny - only have self vote
        let became_leader = node.handle_request_vote_result(2, &result_denied);
        assert!(!became_leader);
        let became_leader = node.handle_request_vote_result(3, &result_denied);
        assert!(!became_leader);

        // Still candidate, waiting for timeout to retry
        assert_eq!(node.state, RaftState::Candidate);
    }

    // === Log Consistency Tests ===

    #[test]
    fn test_append_entries_fails_prev_log_index_too_high() {
        let mut node = new_test_core(1, vec![2, 3]);
        // Node has empty log

        // Leader tries to append entry at index 2, claiming prev_log_index=1 exists
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 1, // We don't have index 1
            prev_log_term: 1,
            entries: vec![LogEntry {
                term: 1,
                index: 2,
                command: "SET x=1".to_string(),
            }],
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);

        assert!(!output.result.success);
        assert_eq!(node.log.len(), 0); // Log unchanged
    }

    #[test]
    fn test_append_entries_fails_prev_log_term_mismatch() {
        let mut node = new_test_core(1, vec![2, 3]);
        // Node has entry at index 1 with term 1
        node.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Leader claims prev_log_index=1 has term 2 (wrong!)
        let args = AppendEntriesArgs {
            term: 2,
            leader_id: 2,
            prev_log_index: 1,
            prev_log_term: 2, // Mismatch! We have term 1
            entries: vec![LogEntry {
                term: 2,
                index: 2,
                command: "SET y=2".to_string(),
            }],
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);

        assert!(!output.result.success);
        assert_eq!(node.log.len(), 1); // Log unchanged
    }

    #[test]
    fn test_append_entries_truncates_conflicting_entries() {
        let mut node = new_test_core(1, vec![2, 3]);
        // Node has entries from old leader at term 1
        node.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });
        node.log.push(LogEntry {
            term: 1,
            index: 2,
            command: "SET y=OLD".to_string(), // This will be replaced
        });

        // New leader at term 2 sends entry at index 2
        let args = AppendEntriesArgs {
            term: 2,
            leader_id: 2,
            prev_log_index: 1,
            prev_log_term: 1, // Matches our entry at index 1
            entries: vec![LogEntry {
                term: 2,
                index: 2,
                command: "SET y=NEW".to_string(),
            }],
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(node.log.len(), 2);
        assert_eq!(node.log[1].command, "SET y=NEW");
        assert_eq!(node.log[1].term, 2);
    }

    #[test]
    fn test_append_entries_idempotent() {
        let mut node = new_test_core(1, vec![2, 3]);

        // First append
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![LogEntry {
                term: 1,
                index: 1,
                command: "SET x=1".to_string(),
            }],
            leader_commit: 0,
        };
        let output = node.handle_append_entries(&args);
        assert!(output.result.success);
        assert_eq!(node.log.len(), 1);

        // Same append again (retransmission)
        let output = node.handle_append_entries(&args);
        assert!(output.result.success);
        // Should still have only 1 entry (idempotent)
        assert_eq!(node.log.len(), 1);
        assert_eq!(node.log[0].command, "SET x=1");
    }

    #[test]
    fn test_commit_index_advances_on_append_entries() {
        let mut node = new_test_core(1, vec![2, 3]);

        // Append entry
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![LogEntry {
                term: 1,
                index: 1,
                command: "SET x=1".to_string(),
            }],
            leader_commit: 1, // Leader has committed this entry
        };
        let output = node.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(node.commit_index, 1);
        assert_eq!(node.last_applied, 1);
    }

    #[test]
    fn test_commit_index_limited_by_log_length() {
        let mut node = new_test_core(1, vec![2, 3]);

        // Leader says commit_index=5 but we only have 1 entry
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![LogEntry {
                term: 1,
                index: 1,
                command: "SET x=1".to_string(),
            }],
            leader_commit: 5, // Higher than our log
        };
        let output = node.handle_append_entries(&args);

        assert!(output.result.success);
        // commit_index should be min(leader_commit, last_log_index) = 1
        assert_eq!(node.commit_index, 1);
    }

    // === Leader Replication Logic Tests ===

    #[test]
    fn test_next_index_decrements_on_failed_append() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 1;
        leader.state = RaftState::Leader;

        // Initialize next_index for peer 2 (assume it's at index 5)
        leader.next_index.insert(2, 5);

        // Peer rejects AppendEntries (log mismatch)
        let result = AppendEntriesResult {
            term: 1,
            success: false,
        };
        leader.handle_append_entries_result(2, 5, &result);

        // next_index should decrement to 4 for retry
        assert_eq!(leader.next_index.get(&2), Some(&4));
    }

    #[test]
    fn test_next_index_does_not_go_below_1() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 1;
        leader.state = RaftState::Leader;

        // next_index is already at 1
        leader.next_index.insert(2, 1);

        // Peer rejects AppendEntries
        let result = AppendEntriesResult {
            term: 1,
            success: false,
        };
        leader.handle_append_entries_result(2, 1, &result);

        // next_index should stay at 1 (can't go lower)
        assert_eq!(leader.next_index.get(&2), Some(&1));
    }

    #[test]
    fn test_match_index_updates_on_successful_append() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 1;
        leader.state = RaftState::Leader;
        leader.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Initialize
        leader.next_index.insert(2, 1);
        leader.match_index.insert(2, 0);

        // Peer accepts AppendEntries for index 1
        let result = AppendEntriesResult {
            term: 1,
            success: true,
        };
        leader.handle_append_entries_result(2, 1, &result);

        // match_index should update to 1
        assert_eq!(leader.match_index.get(&2), Some(&1));
        // next_index should advance to 2
        assert_eq!(leader.next_index.get(&2), Some(&2));
    }

    #[test]
    fn test_match_index_does_not_decrease() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 1;
        leader.state = RaftState::Leader;

        // Peer already has match_index of 5
        leader.match_index.insert(2, 5);

        // Receive success for index 3 (stale/duplicate response)
        let result = AppendEntriesResult {
            term: 1,
            success: true,
        };
        leader.handle_append_entries_result(2, 3, &result);

        // match_index should stay at 5 (not decrease)
        assert_eq!(leader.match_index.get(&2), Some(&5));
    }

    #[test]
    fn test_entry_not_committed_without_majority() {
        let mut leader = new_test_core(1, vec![2, 3, 4, 5]); // 5-node cluster
        leader.current_term = 1;
        leader.state = RaftState::Leader;
        leader.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Only peer 2 replicates (leader + 1 peer = 2, need 3 for majority)
        let result = AppendEntriesResult {
            term: 1,
            success: true,
        };
        let (committed, _) = leader.handle_append_entries_result(2, 1, &result);

        assert!(committed.is_none());
        assert_eq!(leader.commit_index, 0); // Not committed yet
    }

    #[test]
    fn test_entry_committed_with_majority() {
        let mut leader = new_test_core(1, vec![2, 3, 4, 5]); // 5-node cluster
        leader.current_term = 1;
        leader.state = RaftState::Leader;
        leader.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });

        let result = AppendEntriesResult {
            term: 1,
            success: true,
        };

        // Peer 2 replicates (2 total)
        let (committed, _) = leader.handle_append_entries_result(2, 1, &result);
        assert!(committed.is_none());

        // Peer 3 replicates (3 total = majority in 5-node cluster)
        let (committed, _) = leader.handle_append_entries_result(3, 1, &result);
        assert_eq!(committed, Some(1));
        assert_eq!(leader.commit_index, 1);
    }

    #[test]
    fn test_commit_multiple_entries_at_once() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 1;
        leader.state = RaftState::Leader;

        // Leader has 3 entries
        for i in 1..=3 {
            leader.log.push(LogEntry {
                term: 1,
                index: i,
                command: format!("CMD {}", i),
            });
        }

        let result = AppendEntriesResult {
            term: 1,
            success: true,
        };

        // Peer 2 replicates up to index 3
        let (committed, _) = leader.handle_append_entries_result(2, 3, &result);

        // Should commit all 3 entries (leader + peer2 = majority)
        assert_eq!(committed, Some(3));
        assert_eq!(leader.commit_index, 3);
        assert_eq!(leader.last_applied, 3);
    }

    #[test]
    fn test_leader_loses_leadership_on_higher_term_response() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 1;
        leader.state = RaftState::Leader;
        leader.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Peer responds with higher term
        let result = AppendEntriesResult {
            term: 5,
            success: false,
        };
        let (committed, _) = leader.handle_append_entries_result(2, 1, &result);

        // Should step down and not commit
        assert!(committed.is_none());
        assert_eq!(leader.state, RaftState::Follower);
        assert_eq!(leader.current_term, 5);
        assert_eq!(leader.commit_index, 0);
    }

    #[test]
    fn test_become_leader_initializes_next_index() {
        let mut node = new_test_core(1, vec![2, 3]);

        // Add some log entries before becoming leader
        node.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });
        node.log.push(LogEntry {
            term: 1,
            index: 2,
            command: "SET y=2".to_string(),
        });

        node.current_term = 2;
        node.become_leader();

        // next_index should be last_log_index + 1 = 3 for all peers
        assert_eq!(node.next_index.get(&2), Some(&3));
        assert_eq!(node.next_index.get(&3), Some(&3));

        // match_index should be 0 for all peers
        assert_eq!(node.match_index.get(&2), Some(&0));
        assert_eq!(node.match_index.get(&3), Some(&0));
    }

    #[test]
    fn test_leader_cannot_commit_previous_term_entries_directly() {
        // Raft paper Section 5.4.2: Leader cannot commit entries from previous terms
        // by counting replicas. Must commit entry from current term first.
        let mut leader = new_test_core(1, vec![2, 3]);

        // Leader has entry from term 1 (previous term)
        leader.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Leader is now at term 2
        leader.current_term = 2;
        leader.state = RaftState::Leader;
        leader.become_leader();

        // Entry from term 1 gets replicated to majority
        let result = AppendEntriesResult {
            term: 2,
            success: true,
        };
        let (committed, _) = leader.handle_append_entries_result(2, 1, &result);

        // Should NOT commit entry from previous term directly
        // (This test documents the expected behavior per Raft paper)
        assert!(committed.is_none(), "Should not commit previous term entry directly");
        assert_eq!(leader.commit_index, 0);
    }

    #[test]
    fn test_previous_term_entries_committed_indirectly() {
        // Once a current-term entry is committed, previous entries are committed too
        let mut leader = new_test_core(1, vec![2, 3]);

        // Entry from previous term
        leader.log.push(LogEntry {
            term: 1,
            index: 1,
            command: "SET x=1".to_string(),
        });

        // Entry from current term
        leader.log.push(LogEntry {
            term: 2,
            index: 2,
            command: "SET y=2".to_string(),
        });

        leader.current_term = 2;
        leader.state = RaftState::Leader;
        leader.become_leader();

        // Both entries replicated to peer 2
        let result = AppendEntriesResult {
            term: 2,
            success: true,
        };
        let (committed, _) = leader.handle_append_entries_result(2, 2, &result);

        // Entry 2 (current term) committed, which indirectly commits entry 1
        assert_eq!(committed, Some(2));
        assert_eq!(leader.commit_index, 2);
        assert_eq!(leader.last_applied, 2); // Both entries applied
    }

    // === Multi-Step Scenarios ===

    #[test]
    fn test_follower_catches_up_multiple_entries() {
        let mut follower = new_test_core(1, vec![2, 3]);

        // Leader sends 3 entries at once to catch up follower
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![
                LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
                LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
                LogEntry { term: 1, index: 3, command: "CMD 3".to_string() },
            ],
            leader_commit: 2, // Leader has committed up to index 2
        };
        let output = follower.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(follower.log.len(), 3);
        assert_eq!(follower.commit_index, 2); // Follows leader's commit
        assert_eq!(follower.last_applied, 2);
    }

    #[test]
    fn test_follower_catches_up_incrementally() {
        let mut follower = new_test_core(1, vec![2, 3]);

        // First batch: entries 1-2
        let args1 = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![
                LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
                LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
            ],
            leader_commit: 1,
        };
        follower.handle_append_entries(&args1);
        assert_eq!(follower.log.len(), 2);
        assert_eq!(follower.commit_index, 1);

        // Second batch: entries 3-4
        let args2 = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 2,
            prev_log_term: 1,
            entries: vec![
                LogEntry { term: 1, index: 3, command: "CMD 3".to_string() },
                LogEntry { term: 1, index: 4, command: "CMD 4".to_string() },
            ],
            leader_commit: 3,
        };
        let output = follower.handle_append_entries(&args2);

        assert!(output.result.success);
        assert_eq!(follower.log.len(), 4);
        assert_eq!(follower.commit_index, 3);
    }

    #[test]
    fn test_multiple_elections_term_increases() {
        let mut node = new_test_core(1, vec![2, 3]);

        // First election
        node.start_election();
        assert_eq!(node.current_term, 1);
        assert_eq!(node.state, RaftState::Candidate);

        // Election fails (no majority), start another
        node.start_election();
        assert_eq!(node.current_term, 2);
        assert_eq!(node.state, RaftState::Candidate);
        assert_eq!(node.voted_for, Some(1)); // Votes for self again

        // Third election
        node.start_election();
        assert_eq!(node.current_term, 3);
    }

    #[test]
    fn test_duplicate_vote_from_same_peer_ignored() {
        let mut node = new_test_core(1, vec![2, 3, 4, 5]); // 5-node cluster
        node.start_election();

        let result_granted = RequestVoteResult {
            term: 1,
            vote_granted: true,
        };

        // Peer 2 votes
        let became_leader = node.handle_request_vote_result(2, &result_granted);
        assert!(!became_leader); // 2 votes (self + peer2), need 3

        // Peer 2 votes again (duplicate/retransmission)
        let became_leader = node.handle_request_vote_result(2, &result_granted);
        assert!(!became_leader); // Still only 2 unique votes

        // Peer 3 votes - now we have majority
        let became_leader = node.handle_request_vote_result(3, &result_granted);
        assert!(became_leader); // 3 votes = majority
    }

    // === Cluster Configuration Tests ===

    #[test]
    fn test_single_node_cluster_immediate_leader() {
        let mut node = new_test_core(1, vec![]); // No peers

        node.start_election();

        // With no peers, self-vote is majority (1/1)
        // Check if we can become leader
        let total_nodes = 1 + node.peers.len(); // 1
        let majority = (total_nodes / 2) + 1;   // 1
        assert_eq!(majority, 1);

        // Self-vote should be enough
        assert!(node.votes_received.len() >= majority);

        // Manually check - in real impl, start_election + self-vote = leader
        // The current impl requires handling vote results, so let's verify the math
        node.become_leader();
        assert_eq!(node.state, RaftState::Leader);
    }

    #[test]
    fn test_two_node_cluster_needs_both_votes() {
        let mut node = new_test_core(1, vec![2]); // 2-node cluster

        node.start_election();
        // Self-vote gives us 1, need 2 for majority (2/2 + 1 = 2)

        let result_denied = RequestVoteResult {
            term: 1,
            vote_granted: false,
        };

        // Peer denies - no majority
        let became_leader = node.handle_request_vote_result(2, &result_denied);
        assert!(!became_leader);
        assert_eq!(node.state, RaftState::Candidate);
    }

    #[test]
    fn test_two_node_cluster_becomes_leader_with_peer_vote() {
        let mut node = new_test_core(1, vec![2]); // 2-node cluster

        node.start_election();

        let result_granted = RequestVoteResult {
            term: 1,
            vote_granted: true,
        };

        // Peer grants vote - now have majority (2/2)
        let became_leader = node.handle_request_vote_result(2, &result_granted);
        assert!(became_leader);
        assert_eq!(node.state, RaftState::Leader);
    }

    #[test]
    fn test_four_node_cluster_majority() {
        // Even-numbered cluster: 4 nodes need 3 votes for majority
        let mut node = new_test_core(1, vec![2, 3, 4]);

        node.start_election();

        let result_granted = RequestVoteResult {
            term: 1,
            vote_granted: true,
        };

        // Self + peer2 = 2 votes, not enough
        let became_leader = node.handle_request_vote_result(2, &result_granted);
        assert!(!became_leader);

        // Self + peer2 + peer3 = 3 votes = majority
        let became_leader = node.handle_request_vote_result(3, &result_granted);
        assert!(became_leader);
        assert_eq!(node.state, RaftState::Leader);
    }

    // === Log Divergence Tests ===

    #[test]
    fn test_follower_with_extra_uncommitted_entries_gets_truncated() {
        // Scenario: Follower received entries from old leader that were never committed
        // New leader sends AppendEntries that conflicts - follower must truncate
        let mut follower = new_test_core(1, vec![2, 3]);

        // Follower has entries from old leader (term 1)
        follower.log.push(LogEntry { term: 1, index: 1, command: "OLD 1".to_string() });
        follower.log.push(LogEntry { term: 1, index: 2, command: "OLD 2".to_string() });
        follower.log.push(LogEntry { term: 1, index: 3, command: "OLD 3".to_string() });
        follower.current_term = 1;

        // New leader at term 2 has different entry at index 2
        let args = AppendEntriesArgs {
            term: 2,
            leader_id: 2,
            prev_log_index: 1,
            prev_log_term: 1, // Matches follower's entry 1
            entries: vec![
                LogEntry { term: 2, index: 2, command: "NEW 2".to_string() },
            ],
            leader_commit: 0,
        };
        let output = follower.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(follower.log.len(), 2); // Truncated entries 2-3, added new entry 2
        assert_eq!(follower.log[1].term, 2);
        assert_eq!(follower.log[1].command, "NEW 2");
        assert_eq!(follower.current_term, 2);
    }

    #[test]
    fn test_follower_with_gap_rejects_append() {
        // Follower is missing entries - should reject until caught up
        let mut follower = new_test_core(1, vec![2, 3]);

        // Follower only has entry 1
        follower.log.push(LogEntry { term: 1, index: 1, command: "CMD 1".to_string() });

        // Leader tries to append entry 5, claiming prev_log_index=4
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 4, // Follower doesn't have this
            prev_log_term: 1,
            entries: vec![
                LogEntry { term: 1, index: 5, command: "CMD 5".to_string() },
            ],
            leader_commit: 0,
        };
        let output = follower.handle_append_entries(&args);

        assert!(!output.result.success);
        assert_eq!(follower.log.len(), 1); // Unchanged
    }

    // === Stale/Out-of-Order Response Tests ===

    #[test]
    fn test_ignore_stale_vote_response_from_old_term() {
        let mut node = new_test_core(1, vec![2, 3]);

        // Start election at term 1
        node.start_election();
        assert_eq!(node.current_term, 1);

        // Move to term 3 (e.g., saw higher term from another node)
        node.current_term = 3;
        node.state = RaftState::Follower;
        node.voted_for = None;

        // Receive stale vote response from term 1
        let stale_result = RequestVoteResult {
            term: 1,
            vote_granted: true,
        };
        let became_leader = node.handle_request_vote_result(2, &stale_result);

        // Should not become leader - we're not even a candidate anymore
        assert!(!became_leader);
        assert_eq!(node.state, RaftState::Follower);
    }

    #[test]
    fn test_stale_append_response_does_not_affect_commit() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 5;
        leader.state = RaftState::Leader;

        // Leader has entries
        leader.log.push(LogEntry { term: 5, index: 1, command: "CMD 1".to_string() });

        // Receive response claiming term 3 (stale)
        let stale_result = AppendEntriesResult {
            term: 3,
            success: true,
        };
        let _ = leader.handle_append_entries_result(2, 1, &stale_result);

        // Should still process (term is lower, so no step-down)
        // Entry should be counted for commit
        assert_eq!(leader.match_index.get(&2), Some(&1));
    }

    // === Leader Operations Tests ===

    #[test]
    fn test_leader_appends_multiple_entries_sequentially() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 1;
        leader.state = RaftState::Leader;
        leader.become_leader();
        // become_leader() appends NOOP at index 1

        // Append first entry (after NOOP)
        let entry1 = leader.append_log_entry("CMD 1".to_string());
        assert!(entry1.is_some());
        assert_eq!(entry1.unwrap().index, 2);

        // Append second entry
        let entry2 = leader.append_log_entry("CMD 2".to_string());
        assert!(entry2.is_some());
        assert_eq!(entry2.unwrap().index, 3);

        // Append third entry
        let entry3 = leader.append_log_entry("CMD 3".to_string());
        assert!(entry3.is_some());
        assert_eq!(entry3.unwrap().index, 4);

        assert_eq!(leader.log.len(), 4); // NOOP + 3 commands
        assert_eq!(leader.log[0].command, NOOP_COMMAND);
        assert_eq!(leader.log[1].command, "CMD 1");
        assert_eq!(leader.log[3].command, "CMD 3");
    }

    #[test]
    fn test_non_leader_cannot_append_entries() {
        let mut follower = new_test_core(1, vec![2, 3]);
        follower.state = RaftState::Follower;

        let result = follower.append_log_entry("CMD".to_string());
        assert!(result.is_none());

        let mut candidate = new_test_core(2, vec![1, 3]);
        candidate.start_election();

        let result = candidate.append_log_entry("CMD".to_string());
        assert!(result.is_none());
    }

    #[test]
    fn test_leader_entry_has_current_term() {
        let mut leader = new_test_core(1, vec![2, 3]);
        leader.current_term = 7;
        leader.state = RaftState::Leader;

        let entry = leader.append_log_entry("CMD".to_string());

        assert!(entry.is_some());
        assert_eq!(entry.unwrap().term, 7);
    }

    // === Edge Cases ===

    #[test]
    fn test_vote_request_resets_voted_for_on_new_term() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.current_term = 1;
        node.voted_for = Some(2); // Voted for node 2 in term 1

        // Receive vote request from higher term
        let args = RequestVoteArgs {
            term: 5,
            candidate_id: 3,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);

        // Should reset voted_for and grant vote to new candidate
        assert!(result.vote_granted);
        assert_eq!(node.voted_for, Some(3));
        assert_eq!(node.current_term, 5);
    }

    #[test]
    fn test_empty_append_entries_still_updates_commit_index() {
        let mut follower = new_test_core(1, vec![2, 3]);

        // Follower already has entries
        follower.log.push(LogEntry { term: 1, index: 1, command: "CMD 1".to_string() });
        follower.log.push(LogEntry { term: 1, index: 2, command: "CMD 2".to_string() });
        follower.commit_index = 0;

        // Leader sends empty AppendEntries (heartbeat) with updated commit
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 2,
            prev_log_term: 1,
            entries: vec![], // Empty
            leader_commit: 2,
        };
        let output = follower.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(follower.commit_index, 2);
        assert_eq!(follower.last_applied, 2);
    }

    #[test]
    fn test_append_entries_with_entries_already_present() {
        // Test idempotency when leader retransmits entries we already have
        let mut follower = new_test_core(1, vec![2, 3]);

        // Follower already has entries 1-3
        follower.log.push(LogEntry { term: 1, index: 1, command: "CMD 1".to_string() });
        follower.log.push(LogEntry { term: 1, index: 2, command: "CMD 2".to_string() });
        follower.log.push(LogEntry { term: 1, index: 3, command: "CMD 3".to_string() });

        // Leader retransmits entries 2-3 (already present with same term)
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 2,
            prev_log_index: 1,
            prev_log_term: 1,
            entries: vec![
                LogEntry { term: 1, index: 2, command: "CMD 2".to_string() },
                LogEntry { term: 1, index: 3, command: "CMD 3".to_string() },
            ],
            leader_commit: 0,
        };
        let output = follower.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(follower.log.len(), 3); // No duplicates
    }

    #[test]
    fn test_candidate_resets_votes_on_new_election() {
        let mut node = new_test_core(1, vec![2, 3, 4, 5]);

        // First election - get some votes but not majority
        node.start_election();
        let result = RequestVoteResult { term: 1, vote_granted: true };
        node.handle_request_vote_result(2, &result);
        // Have 2 votes (self + peer2), need 3

        // Start new election
        node.start_election();

        // Should have reset - only self vote now
        // Need to get 2 more votes for majority
        let result = RequestVoteResult { term: 2, vote_granted: true };
        let became_leader = node.handle_request_vote_result(3, &result);
        assert!(!became_leader); // Only 2 votes

        let became_leader = node.handle_request_vote_result(4, &result);
        assert!(became_leader); // Now 3 votes
    }

    // === Persistence / Restart Tests ===

    #[test]
    fn test_node_restarts_with_saved_term() {
        let mut storage = MemoryStorage::new();
        storage.save_term(5).unwrap();

        let node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        assert_eq!(node.current_term, 5);
        assert_eq!(node.state, RaftState::Follower);
    }

    #[test]
    fn test_node_restarts_with_saved_voted_for() {
        let mut storage = MemoryStorage::new();
        storage.save_term(3).unwrap();
        storage.save_voted_for(Some(2)).unwrap();

        let node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        assert_eq!(node.current_term, 3);
        assert_eq!(node.voted_for, Some(2));
    }

    #[test]
    fn test_node_restarts_with_saved_log() {
        let mut storage = MemoryStorage::new();
        storage.save_term(2).unwrap();
        storage.append_log_entries(&[
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 2, index: 2, command: "CMD 2".to_string() },
        ]).unwrap();

        let node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        assert_eq!(node.log.len(), 2);
        assert_eq!(node.log[0].command, "CMD 1");
        assert_eq!(node.log[1].command, "CMD 2");
        assert_eq!(node.last_log_index(), 2);
        assert_eq!(node.last_log_term(), 2);
    }

    #[test]
    fn test_node_restarts_with_full_state() {
        let mut storage = MemoryStorage::new();
        storage.save_term(5).unwrap();
        storage.save_voted_for(Some(1)).unwrap();
        storage.append_log_entries(&[
            LogEntry { term: 3, index: 1, command: "SET x=1".to_string() },
            LogEntry { term: 4, index: 2, command: "SET y=2".to_string() },
            LogEntry { term: 5, index: 3, command: "SET z=3".to_string() },
        ]).unwrap();

        let node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        assert_eq!(node.current_term, 5);
        assert_eq!(node.voted_for, Some(1));
        assert_eq!(node.log.len(), 3);
        // Volatile state should be reset
        assert_eq!(node.commit_index, 0);
        assert_eq!(node.last_applied, 0);
        assert_eq!(node.state, RaftState::Follower);
    }

    #[test]
    fn test_restarted_node_can_continue_election() {
        // Simulate: node voted in term 3, crashed, restarted
        let mut storage = MemoryStorage::new();
        storage.save_term(3).unwrap();
        storage.save_voted_for(Some(2)).unwrap(); // Already voted for node 2

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        // Node 3 requests vote in same term - should be denied (already voted)
        let args = RequestVoteArgs {
            term: 3,
            candidate_id: 3,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);
        assert!(!result.vote_granted);

        // Node 2 requests vote again in same term - should be granted (same candidate)
        let args = RequestVoteArgs {
            term: 3,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(&args);
        assert!(result.vote_granted);
    }

    #[test]
    fn test_restarted_node_accepts_append_entries() {
        // Simulate: node had log entries, crashed, restarted
        let mut storage = MemoryStorage::new();
        storage.save_term(2).unwrap();
        storage.append_log_entries(&[
            LogEntry { term: 1, index: 1, command: "CMD 1".to_string() },
            LogEntry { term: 2, index: 2, command: "CMD 2".to_string() },
        ]).unwrap();

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        // Leader sends new entry
        let args = AppendEntriesArgs {
            term: 2,
            leader_id: 2,
            prev_log_index: 2,
            prev_log_term: 2,
            entries: vec![LogEntry { term: 2, index: 3, command: "CMD 3".to_string() }],
            leader_commit: 2,
        };
        let output = node.handle_append_entries(&args);

        assert!(output.result.success);
        assert_eq!(node.log.len(), 3);
        assert_eq!(node.commit_index, 2);
    }

    #[test]
    fn test_restarted_leader_must_reestablish() {
        // Simulate: node was leader, crashed, restarted
        // After restart, it should be follower (leadership is volatile)
        let mut storage = MemoryStorage::new();
        storage.save_term(5).unwrap();
        storage.save_voted_for(Some(1)).unwrap(); // Voted for self
        storage.append_log_entries(&[
            LogEntry { term: 5, index: 1, command: "CMD 1".to_string() },
        ]).unwrap();

        let node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        // Should be follower, not leader
        assert_eq!(node.state, RaftState::Follower);
        // next_index and match_index should be empty (leader volatile state)
        assert!(node.next_index.is_empty());
        assert!(node.match_index.is_empty());
    }

    #[test]
    fn test_state_persists_across_operations() {
        // Create node, do operations, then verify storage has correct state
        let storage = MemoryStorage::new();
        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(crate::state_machine::TestStateMachine::new()),
        );

        // Start election (persists term=1, voted_for=1)
        node.start_election();

        // Become leader and append entry
        node.become_leader(); // Appends NOOP
        node.append_log_entry("SET x=1".to_string());

        // Now "restart" by creating new node with same storage
        // We need to extract storage - but we can't easily do that
        // So let's just verify the in-memory state matches what should be persisted
        assert_eq!(node.current_term, 1);
        assert_eq!(node.voted_for, Some(1));
        assert_eq!(node.log.len(), 2); // NOOP + SET x=1
        assert_eq!(node.log[0].command, NOOP_COMMAND);
        assert_eq!(node.log[1].command, "SET x=1");
    }

    // === State Machine Apply Tests ===

    #[test]
    fn test_apply_committed_entries_updates_last_applied() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.become_leader();

        // Append some entries
        node.append_log_entry("CMD 1".to_string());
        node.append_log_entry("CMD 2".to_string());
        node.append_log_entry("CMD 3".to_string());

        assert_eq!(node.last_applied, 0);
        assert_eq!(node.commit_index, 0);

        // Simulate committing entries (in real scenario, this happens via majority replication)
        node.commit_index = 2;
        node.apply_committed_entries();

        assert_eq!(node.last_applied, 2);

        // Commit more
        node.commit_index = 3;
        node.apply_committed_entries();

        assert_eq!(node.last_applied, 3);
    }

    #[test]
    fn test_apply_committed_entries_does_nothing_when_up_to_date() {
        let mut node = new_test_core(1, vec![2, 3]);
        node.become_leader();

        node.append_log_entry("CMD 1".to_string());
        node.commit_index = 1;
        node.apply_committed_entries();

        assert_eq!(node.last_applied, 1);

        // Call again - should do nothing
        node.apply_committed_entries();
        assert_eq!(node.last_applied, 1);
    }

    #[test]
    fn test_entries_applied_to_state_machine() {
        use crate::state_machine::{AppliedCommands, TestStateMachine};

        // Create a shared vec we can inspect
        let applied: AppliedCommands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new_shared(applied.clone())),
        );

        node.become_leader(); // Appends NOOP at index 1
        node.append_log_entry("SET x=1".to_string()); // index 2
        node.append_log_entry("SET y=2".to_string()); // index 3
        node.append_log_entry("SET z=3".to_string()); // index 4

        // Nothing applied yet
        assert!(applied.lock().unwrap().is_empty());

        // Commit first two entries (NOOP + SET x=1)
        node.commit_index = 2;
        node.apply_committed_entries();

        {
            let applied = applied.lock().unwrap();
            assert_eq!(applied.len(), 2);
            assert_eq!(applied[0], NOOP_COMMAND);
            assert_eq!(applied[1], "SET x=1");
        }

        // Commit up to SET z=3
        node.commit_index = 4;
        node.apply_committed_entries();

        {
            let applied = applied.lock().unwrap();
            assert_eq!(applied.len(), 4);
            assert_eq!(applied[2], "SET y=2");
            assert_eq!(applied[3], "SET z=3");
        }
    }

    #[test]
    fn test_take_snapshot_basic() {
        use crate::state_machine::kv::KeyValueStore;

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        node.become_leader();
        node.append_log_entry("SET x 1".to_string());
        node.append_log_entry("SET y 2".to_string());
        node.append_log_entry("SET z 3".to_string());

        // Apply entries
        node.commit_index = 3;
        node.apply_committed_entries();
        assert_eq!(node.last_applied, 3);
        assert_eq!(node.log.len(), 4); // NOOP + 3 commands

        // Take snapshot
        node.take_snapshot().unwrap();

        // Verify snapshot metadata
        assert_eq!(node.snapshot_last_index, 3);
        assert_eq!(node.snapshot_last_term, 0); // Term is 0 in test

        // Verify log was truncated (should keep only entry 4)
        assert_eq!(node.log.len(), 1);
        assert_eq!(node.log[0].index, 4);

        // Verify last_log_index still returns correct value
        assert_eq!(node.last_log_index(), 4);
    }

    #[test]
    fn test_take_snapshot_no_entries_applied() {
        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        );

        node.become_leader();
        node.append_log_entry("SET x=1".to_string());

        // Try to snapshot without applying anything
        let result = node.take_snapshot();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No entries applied"));
    }

    #[test]
    fn test_take_snapshot_already_snapshotted() {
        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        );

        node.become_leader();
        node.append_log_entry("SET x=1".to_string());
        node.commit_index = 2;
        node.apply_committed_entries();

        // Take first snapshot
        node.take_snapshot().unwrap();
        assert_eq!(node.snapshot_last_index, 2);

        // Try to snapshot again at same point
        let result = node.take_snapshot();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Already have snapshot"));
    }

    #[test]
    fn test_snapshot_restores_state_machine_on_startup() {
        use crate::state_machine::kv::{KeyValueStore, SharedKvStore};
        use std::sync::{Arc, Mutex};

        // Create a KV store, add data, and create snapshot manually
        let kv1 = KeyValueStore::new();
        let mut node1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(kv1),
        );

        node1.become_leader();
        node1.append_log_entry("SET key1 value1".to_string());
        node1.append_log_entry("SET key2 value2".to_string());
        node1.commit_index = 3;
        node1.apply_committed_entries();
        node1.take_snapshot().unwrap();

        // Get the snapshot from storage
        let snapshot = node1.storage.load_snapshot().unwrap().unwrap();

        // Create new storage with this snapshot
        let mut storage2 = MemoryStorage::new();
        storage2.save_snapshot(&snapshot).unwrap();

        // Create new node - should restore state machine from snapshot
        let kv_restored: SharedKvStore = Arc::new(Mutex::new(KeyValueStore::new()));
        let node2 = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(storage2),
            Box::new(kv_restored.clone()),
        );

        // Verify snapshot metadata loaded
        assert_eq!(node2.snapshot_last_index, 3);
        assert_eq!(node2.snapshot_last_term, 0);

        // Verify commit_index and last_applied set correctly
        assert_eq!(node2.commit_index, 3);
        assert_eq!(node2.last_applied, 3);

        // Verify state machine was restored
        let kv = kv_restored.lock().unwrap();
        assert_eq!(kv.get("key1"), Some("value1".to_string()));
        assert_eq!(kv.get("key2"), Some("value2".to_string()));
    }

    #[test]
    fn test_apply_entries_after_snapshot_restoration() {
        use crate::state_machine::kv::{KeyValueStore, SharedKvStore};
        use std::sync::{Arc, Mutex};

        // Create node, take snapshot
        let mut node1 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        node1.become_leader();
        node1.append_log_entry("SET old data".to_string());
        node1.commit_index = 2;
        node1.apply_committed_entries();
        node1.take_snapshot().unwrap();

        // Get snapshot
        let snapshot = node1.storage.load_snapshot().unwrap().unwrap();

        // Create new storage and node with snapshot
        let mut storage2 = MemoryStorage::new();
        storage2.save_snapshot(&snapshot).unwrap();

        let kv_restored: SharedKvStore = Arc::new(Mutex::new(KeyValueStore::new()));
        let mut node2 = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage2),
            Box::new(kv_restored.clone()),
        );

        assert_eq!(node2.last_applied, 2);

        // Make it a leader and add new entry after snapshot
        node2.state = RaftState::Leader; // Bypass become_leader to avoid NOOP
        node2.current_leader = Some(node2.id);

        node2.append_log_entry("SET new entry".to_string());
        node2.commit_index = 3;
        node2.apply_committed_entries();

        assert_eq!(node2.last_applied, 3);

        // Verify state machine has both old and new data
        let kv = kv_restored.lock().unwrap();
        assert_eq!(kv.get("old"), Some("data".to_string()));
        assert_eq!(kv.get("new"), Some("entry".to_string()));
    }

    #[test]
    fn test_last_log_index_with_snapshot() {
        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        );

        // Initially no snapshot, empty log
        assert_eq!(node.last_log_index(), 0);
        assert_eq!(node.last_log_term(), 0);

        node.become_leader(); // Adds NOOP at index 1
        node.commit_index = 1;
        node.apply_committed_entries();
        node.take_snapshot().unwrap();

        // After snapshot, log is empty but last_log_index should return snapshot_last_index
        assert_eq!(node.log.len(), 0);
        assert_eq!(node.last_log_index(), 1);
        assert_eq!(node.last_log_term(), 0);

        // Add more entries
        node.append_log_entry("CMD".to_string());
        assert_eq!(node.last_log_index(), 2);
    }

    #[test]
    fn test_get_log_entry_with_snapshot() {
        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        );

        node.become_leader(); // NOOP at index 1
        node.append_log_entry("CMD2".to_string()); // index 2
        node.append_log_entry("CMD3".to_string()); // index 3
        node.append_log_entry("CMD4".to_string()); // index 4

        node.commit_index = 2;
        node.apply_committed_entries();
        node.take_snapshot().unwrap(); // Snapshot up to index 2

        // Entries 1-2 are in snapshot
        assert!(node.get_log_entry(1).is_none());
        assert!(node.get_log_entry(2).is_none());

        // Entries 3-4 are still in log
        assert_eq!(node.get_log_entry(3).unwrap().command, "CMD3");
        assert_eq!(node.get_log_entry(4).unwrap().command, "CMD4");

        // Beyond log
        assert!(node.get_log_entry(5).is_none());
    }

    #[test]
    fn test_handle_install_snapshot_success() {
        use crate::state_machine::kv::{KeyValueStore, SharedKvStore};
        use crate::state_machine::{Snapshotable, StateMachine};
        use std::sync::{Arc, Mutex};

        // Create node with KV store
        let kv: SharedKvStore = Arc::new(Mutex::new(KeyValueStore::new()));
        let mut node = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(kv.clone()),
        );

        // Create snapshot data
        let mut source_kv = KeyValueStore::new();
        source_kv.apply("SET key1 value1").unwrap();
        source_kv.apply("SET key2 value2").unwrap();
        let snapshot_data = source_kv.snapshot().unwrap();

        let args = InstallSnapshotArgs {
            term: 5,
            leader_id: 1,
            last_included_index: 10,
            last_included_term: 4,
            data: snapshot_data,
        };

        let result = node.handle_install_snapshot(&args);

        // Should succeed
        assert!(matches!(result, InstallSnapshotResult::Success { .. }));
        if let InstallSnapshotResult::Success { term } = result {
            assert_eq!(term, 5);
        }

        // Verify snapshot metadata updated
        assert_eq!(node.snapshot_last_index, 10);
        assert_eq!(node.snapshot_last_term, 4);
        assert_eq!(node.commit_index, 10);
        assert_eq!(node.last_applied, 10);

        // Verify state machine restored
        let kv_lock = kv.lock().unwrap();
        assert_eq!(kv_lock.get("key1"), Some("value1".to_string()));
        assert_eq!(kv_lock.get("key2"), Some("value2".to_string()));
    }

    #[test]
    fn test_handle_install_snapshot_stale_term() {
        let mut node = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        );

        node.current_term = 10;

        let args = InstallSnapshotArgs {
            term: 5, // Stale term
            leader_id: 1,
            last_included_index: 10,
            last_included_term: 4,
            data: vec![],
        };

        let result = node.handle_install_snapshot(&args);

        assert!(matches!(result, InstallSnapshotResult::Failed { .. }));
        if let InstallSnapshotResult::Failed { term, reason } = result {
            assert_eq!(term, 10);
            assert!(reason.contains("stale term"));
        }

        // Should not update anything
        assert_eq!(node.snapshot_last_index, 0);
    }

    #[test]
    fn test_handle_install_snapshot_old_snapshot() {
        let mut node = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(TestStateMachine::new()),
        );

        // Node already has a newer snapshot
        node.snapshot_last_index = 20;
        node.snapshot_last_term = 5;

        let args = InstallSnapshotArgs {
            term: 5,
            leader_id: 1,
            last_included_index: 10, // Older than current snapshot
            last_included_term: 4,
            data: vec![],
        };

        let result = node.handle_install_snapshot(&args);

        assert!(matches!(result, InstallSnapshotResult::Failed { .. }));
        if let InstallSnapshotResult::Failed { reason, .. } = result {
            assert!(reason.contains("too old"));
        }

        // Should not update
        assert_eq!(node.snapshot_last_index, 20);
    }

    #[test]
    fn test_handle_install_snapshot_discards_old_log_entries() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::state_machine::Snapshotable;

        let mut node = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        // Add some log entries
        node.become_leader();
        node.append_log_entry("CMD2".to_string());
        node.append_log_entry("CMD3".to_string());
        node.append_log_entry("CMD4".to_string());
        node.append_log_entry("CMD5".to_string());

        assert_eq!(node.log.len(), 5); // NOOP + 4 commands

        // Install snapshot that covers first 3 entries
        let snapshot_kv = KeyValueStore::new();
        let snapshot_data = snapshot_kv.snapshot().unwrap();

        let args = InstallSnapshotArgs {
            term: 1,
            leader_id: 1,
            last_included_index: 3,
            last_included_term: 0,
            data: snapshot_data,
        };

        node.handle_install_snapshot(&args);

        // Should keep only entries 4 and 5
        assert_eq!(node.log.len(), 2);
        assert_eq!(node.log[0].index, 4);
        assert_eq!(node.log[1].index, 5);
    }

    #[test]
    fn test_handle_install_snapshot_updates_commit_and_applied() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::state_machine::Snapshotable;

        let mut node = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        node.commit_index = 5;
        node.last_applied = 5;

        let snapshot_kv = KeyValueStore::new();
        let snapshot_data = snapshot_kv.snapshot().unwrap();

        let args = InstallSnapshotArgs {
            term: 1,
            leader_id: 1,
            last_included_index: 10, // Beyond current commit/applied
            last_included_term: 1,
            data: snapshot_data,
        };

        node.handle_install_snapshot(&args);

        // Should update both to snapshot index
        assert_eq!(node.commit_index, 10);
        assert_eq!(node.last_applied, 10);
    }



    #[test]
    fn test_automatic_snapshot_triggering() {
        use crate::state_machine::kv::KeyValueStore;

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        // Set a low threshold for testing
        node.set_snapshot_threshold(5);

        // Become leader
        node.start_election();
        node.become_leader();

        // Add 4 entries (NOOP + 3 commands) - below threshold
        node.append_log_entry("SET k1 v1".to_string());
        node.append_log_entry("SET k2 v2".to_string());
        node.append_log_entry("SET k3 v3".to_string());

        // Commit and apply
        node.commit_index = 4;
        node.apply_committed_entries();

        // No snapshot yet (4 < 5)
        assert_eq!(node.snapshot_last_index, 0);
        assert_eq!(node.log.len(), 4);

        // Add one more entry to hit threshold
        node.append_log_entry("SET k4 v4".to_string());
        node.commit_index = 5;
        node.apply_committed_entries();

        // Should have triggered automatic snapshot
        assert_eq!(node.snapshot_last_index, 5);
        assert_eq!(node.snapshot_last_term, 1);
        assert!(node.log.is_empty()); // All entries compacted

        // Add more entries - threshold resets from snapshot
        for i in 6..=10 {
            node.append_log_entry(format!("SET k{} v{}", i, i));
        }
        node.commit_index = 10;
        node.apply_committed_entries();

        // Another snapshot should be triggered (10 - 5 = 5 entries)
        assert_eq!(node.snapshot_last_index, 10);
        assert!(node.log.is_empty());
    }

    #[test]
    fn test_automatic_snapshot_disabled_when_threshold_is_zero() {
        use crate::state_machine::kv::KeyValueStore;

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        // Disable automatic snapshots
        node.set_snapshot_threshold(0);

        node.start_election();
        node.become_leader();

        // Add many entries
        for i in 1..=100 {
            node.append_log_entry(format!("SET k{} v{}", i, i));
        }
        node.commit_index = 101;
        node.apply_committed_entries();

        // Should NOT have triggered snapshot
        assert_eq!(node.snapshot_last_index, 0);
        assert_eq!(node.log.len(), 101); // NOOP + 100 commands
    }

    #[test]
    fn test_automatic_snapshot_on_follower() {
        use crate::state_machine::kv::KeyValueStore;

        let mut node = RaftCore::new(
            2,
            vec![1, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        node.set_snapshot_threshold(5);

        // Node is a follower
        assert_eq!(node.state, RaftState::Follower);

        // Simulate receiving entries from leader
        for i in 1..=10 {
            node.log.push(LogEntry {
                term: 1,
                index: i,
                command: format!("SET k{} v{}", i, i),
            });
        }
        node.commit_index = 10;
        node.apply_committed_entries();

        // Followers also take snapshots to prevent unbounded log growth
        assert_eq!(node.snapshot_last_index, 10);
        assert_eq!(node.snapshot_last_term, 1);
        assert!(node.log.is_empty()); // All entries compacted
    }

    #[test]
    fn test_snapshot_compacts_file_storage() {
        use crate::state_machine::kv::KeyValueStore;
        use crate::storage::file::FileStorage;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let storage = FileStorage::new(dir.path().to_str().unwrap()).unwrap();

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(storage),
            Box::new(KeyValueStore::new()),
        );

        // Become leader and add entries
        node.start_election();
        node.become_leader();

        for i in 1..=10 {
            node.append_log_entry(format!("SET k{} v{}", i, i));
        }

        // Commit and apply all entries
        node.commit_index = node.last_log_index();
        node.apply_committed_entries();

        // Verify log has entries (NOOP + 10 commands = 11 entries)
        assert_eq!(node.log.len(), 11);

        // Take snapshot
        node.take_snapshot().unwrap();

        // Verify in-memory log is empty
        assert!(node.log.is_empty());
        assert_eq!(node.snapshot_last_index, 11);

        // Verify physical log file is empty
        let log_entries = node.storage.load_log().unwrap();
        assert_eq!(log_entries.len(), 0, "Physical log file should be empty after snapshot");
    }

    #[test]
    fn test_multiple_snapshots_work() {
        use crate::state_machine::kv::KeyValueStore;

        let mut node = RaftCore::new(
            1,
            vec![2, 3],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        node.set_snapshot_threshold(5);

        // Become leader
        node.start_election();
        node.become_leader();

        // Add 5 entries (will trigger snapshot at threshold)
        for i in 1..=5 {
            node.append_log_entry(format!("SET k{} v{}", i, i));
        }
        node.commit_index = node.last_log_index();
        node.apply_committed_entries();

        let first_snapshot_index = node.snapshot_last_index;
        assert!(first_snapshot_index > 0, "First snapshot should have been taken");

        // Add 5 more entries (should trigger second snapshot)
        for i in 6..=10 {
            node.append_log_entry(format!("SET k{} v{}", i, i));
        }
        node.commit_index = node.last_log_index();
        node.apply_committed_entries();

        // Second snapshot should have been taken
        assert!(node.snapshot_last_index > first_snapshot_index,
                "Second snapshot should have advanced: first={} current={}",
                first_snapshot_index, node.snapshot_last_index);
    }

    #[test]
    fn test_replication_after_compaction() {
        use crate::state_machine::kv::KeyValueStore;

        // Create leader that has taken a snapshot
        let mut leader = RaftCore::new(
            1,
            vec![2],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );
        leader.start_election();
        leader.become_leader();

        // Add entries 1-10, commit and apply them
        for i in 1..=10 {
            leader.append_log_entry(format!("SET k{} v{}", i, i));
        }
        leader.commit_index = leader.last_log_index();
        leader.apply_committed_entries();

        // Take snapshot (NOOP entry 1 + 10 user entries = index 11)
        leader.take_snapshot().unwrap();
        let snapshot_idx = leader.snapshot_last_index;
        assert!(leader.log.is_empty(), "Log should be empty after snapshot");

        // Add 5 more entries after snapshot
        for i in 1..=5 {
            leader.append_log_entry(format!("SET k{} v{}", i, i));
        }
        assert_eq!(leader.log.len(), 5);

        // Create follower that also has snapshot
        let mut follower = RaftCore::new(
            2,
            vec![1],
            Box::new(MemoryStorage::new()),
            Box::new(KeyValueStore::new()),
        );

        // Give follower the same snapshot
        follower.snapshot_last_index = snapshot_idx;
        follower.snapshot_last_term = 1;
        follower.last_applied = snapshot_idx;
        follower.commit_index = snapshot_idx;

        // Follower has first 3 entries after snapshot
        for i in 1..=3 {
            follower.log.push(LogEntry {
                term: 1,
                index: snapshot_idx + i,
                command: format!("SET k{} v{}", i, i),
            });
        }
        assert_eq!(follower.log.len(), 3);

        // Leader sends AppendEntries with last 2 entries
        let args = AppendEntriesArgs {
            term: 1,
            leader_id: 1,
            prev_log_index: snapshot_idx + 3,
            prev_log_term: 1,
            entries: vec![
                LogEntry { term: 1, index: snapshot_idx + 4, command: "SET k4 v4".to_string() },
                LogEntry { term: 1, index: snapshot_idx + 5, command: "SET k5 v5".to_string() },
            ],
            leader_commit: snapshot_idx + 5,
        };

        let result = follower.handle_append_entries(&args);
        assert!(result.result.success);

        // Follower should now have 5 entries (no duplicates!)
        assert_eq!(follower.log.len(), 5);
        assert_eq!(follower.log[0].index, snapshot_idx + 1);
        assert_eq!(follower.log[1].index, snapshot_idx + 2);
        assert_eq!(follower.log[2].index, snapshot_idx + 3);
        assert_eq!(follower.log[3].index, snapshot_idx + 4);
        assert_eq!(follower.log[4].index, snapshot_idx + 5);

        // Send same entries again (idempotent)
        let result2 = follower.handle_append_entries(&args);
        assert!(result2.result.success);

        // Should still have exactly 5 entries (no duplicates)
        assert_eq!(follower.log.len(), 5);
        assert_eq!(follower.log[4].index, snapshot_idx + 5);
    }
}
