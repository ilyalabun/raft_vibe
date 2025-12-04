//! Raft Consensus Algorithm Implementation
//! 
//! This module implements the Raft consensus protocol as described in:
//! "In Search of an Understandable Consensus Algorithm" by Diego Ongaro and John Ousterhout

use std::collections::HashMap;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Term when entry was received by leader
    pub term: u64,
    /// Index in the log (1-indexed)
    pub index: u64,
    /// Command/data stored in this entry
    pub command: String,
}

/// RequestVote RPC arguments
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct RequestVoteResult {
    /// Current term, for candidate to update itself
    pub term: u64,
    /// True means candidate received vote
    pub vote_granted: bool,
}

/// AppendEntries RPC arguments
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct AppendEntriesResult {
    /// Current term, for leader to update itself
    pub term: u64,
    /// True if follower contained entry matching prev_log_index and prev_log_term
    pub success: bool,
}

/// Core Raft node implementation
pub struct RaftNode {
    // Persistent state on all servers (updated on stable storage before responding to RPCs)
    /// Latest term server has seen (initialized to 0 on first boot, increases monotonically)
    pub current_term: u64,
    /// Candidate ID that received vote in current term (or None if none)
    pub voted_for: Option<u64>,
    /// Log entries; each entry contains command for state machine, and term when entry was received by leader (first index is 1)
    pub log: Vec<LogEntry>,

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
}

impl RaftNode {
    /// Create a new Raft node
    pub fn new(id: u64, peers: Vec<u64>) -> Self {
        RaftNode {
            current_term: 0,
            voted_for: None,
            log: Vec::new(),
            commit_index: 0,
            last_applied: 0,
            next_index: HashMap::new(),
            match_index: HashMap::new(),
            id,
            state: RaftState::Follower,
            peers,
        }
    }

    /// Get the last log index (0 if log is empty)
    pub fn last_log_index(&self) -> u64 {
        if self.log.is_empty() {
            0
        } else {
            self.log.last().unwrap().index
        }
    }

    /// Get the term of the last log entry (0 if log is empty)
    pub fn last_log_term(&self) -> u64 {
        if self.log.is_empty() {
            0
        } else {
            self.log.last().unwrap().term
        }
    }

    /// Check if candidate's log is at least as up-to-date as receiver's log
    /// Returns true if:
    /// - candidate's last log term > receiver's last log term, OR
    /// - candidate's last log term == receiver's last log term AND candidate's last log index >= receiver's last log index
    pub fn is_log_up_to_date(&self, last_log_term: u64, last_log_index: u64) -> bool {
        let my_last_term = self.last_log_term();
        let my_last_index = self.last_log_index();

        last_log_term > my_last_term || 
        (last_log_term == my_last_term && last_log_index >= my_last_index)
    }

    /// Handle RequestVote RPC
    /// Returns (term, vote_granted)
    pub fn handle_request_vote(&mut self, args: RequestVoteArgs) -> RequestVoteResult {
        // If RPC request or response contains term T > currentTerm: set currentTerm = T, convert to follower
        if args.term > self.current_term {
            self.current_term = args.term;
            self.voted_for = None;
            self.state = RaftState::Follower;
        }

        let vote_granted = if args.term < self.current_term {
            // Reply false if term < currentTerm
            false
        } else if self.voted_for.is_some() && self.voted_for != Some(args.candidate_id) {
            // If votedFor is null or candidateId, and candidate's log is at least as up-to-date as receiver's log, grant vote
            false
        } else if !self.is_log_up_to_date(args.last_log_term, args.last_log_index) {
            false
        } else {
            // Grant vote
            self.voted_for = Some(args.candidate_id);
            true
        };

        RequestVoteResult {
            term: self.current_term,
            vote_granted,
        }
    }

    /// Handle AppendEntries RPC (heartbeat or log replication)
    pub fn handle_append_entries(&mut self, args: AppendEntriesArgs) -> AppendEntriesResult {
        // If RPC request or response contains term T > currentTerm: set currentTerm = T, convert to follower
        if args.term > self.current_term {
            self.current_term = args.term;
            self.voted_for = None;
            self.state = RaftState::Follower;
        }

        let success = if args.term < self.current_term {
            // Reply false if term < currentTerm
            false
        } else {
            // If we receive AppendEntries from a valid leader, we're a follower
            // Note: leader_id is used to identify which leader is sending the request
            // In a full implementation, this would be used for leader tracking
            let _leader_id = args.leader_id;
            self.state = RaftState::Follower;

            // Reply false if log doesn't contain an entry at prev_log_index with term matching prev_log_term
            if args.prev_log_index > 0 {
                if args.prev_log_index > self.last_log_index() as u64 {
                    false
                } else {
                    // Check if the entry at prev_log_index has the correct term
                    // Note: log is 0-indexed, but prev_log_index is 1-indexed
                    let log_index = (args.prev_log_index - 1) as usize;
                    if log_index >= self.log.len() || self.log[log_index].term != args.prev_log_term {
                        false
                    } else {
                        // If an existing entry conflicts with a new one (same index but different terms),
                        // delete the existing entry and all that follow it
                        let conflict_index = args.prev_log_index as usize;
                        if conflict_index < self.log.len() {
                            self.log.truncate(conflict_index);
                        }

                        // Append any new entries not already in the log
                        for entry in args.entries {
                            self.log.push(entry);
                        }

                        // If leaderCommit > commitIndex, set commitIndex = min(leaderCommit, index of last new entry)
                        if args.leader_commit > self.commit_index {
                            self.commit_index = std::cmp::min(
                                args.leader_commit,
                                self.last_log_index(),
                            );
                            // Apply committed entries to state machine
                            self.apply_committed_entries();
                        }

                        true
                    }
                }
            } else {
                // prev_log_index is 0, meaning we're starting from the beginning
                // Append any new entries
                for entry in args.entries {
                    self.log.push(entry);
                }

                // Update commit_index
                if args.leader_commit > self.commit_index {
                    self.commit_index = std::cmp::min(
                        args.leader_commit,
                        self.last_log_index(),
                    );
                    // Apply committed entries to state machine
                    self.apply_committed_entries();
                }

                true
            }
        };

        AppendEntriesResult {
            term: self.current_term,
            success,
        }
    }

    /// Start a new election (called when election timeout elapses)
    pub fn start_election(&mut self) {
        // Increment current_term
        self.current_term += 1;
        
        // Transition to candidate
        self.state = RaftState::Candidate;
        
        // Vote for self
        self.voted_for = Some(self.id);
        
        // Reset election timer (in real implementation, this would be handled by a timer)
        // For now, we just update the state
    }

    /// Become leader (called after receiving majority of votes)
    pub fn become_leader(&mut self) {
        self.state = RaftState::Leader;
        
        // Reinitialize next_index and match_index
        let last_index = self.last_log_index();
        for peer_id in &self.peers {
            self.next_index.insert(*peer_id, last_index + 1);
            self.match_index.insert(*peer_id, 0);
        }
    }

    /// Add a new log entry (called by leader when receiving client request)
    pub fn append_log_entry(&mut self, command: String) -> LogEntry {
        let index = self.last_log_index() + 1;
        let entry = LogEntry {
            term: self.current_term,
            index,
            command,
        };
        self.log.push(entry.clone());
        entry
    }

    /// Apply committed entries to the state machine
    /// Updates last_applied to match commit_index
    pub fn apply_committed_entries(&mut self) {
        // In a full implementation, this would apply entries to the state machine
        // For now, we just update last_applied to track what's been applied
        while self.last_applied < self.commit_index {
            self.last_applied += 1;
            // In a real implementation, we would apply self.log[(self.last_applied - 1) as usize] to the state machine
        }
    }

    /// Process a RequestVote response (called by candidate)
    /// Updates term if response contains a higher term
    pub fn process_request_vote_response(&mut self, result: RequestVoteResult) {
        // If RPC response contains term T > currentTerm: set currentTerm = T, convert to follower
        if result.term > self.current_term {
            self.current_term = result.term;
            self.voted_for = None;
            self.state = RaftState::Follower;
        }
    }

    /// Process an AppendEntries response (called by leader)
    /// Updates term if response contains a higher term
    pub fn process_append_entries_response(&mut self, result: AppendEntriesResult) {
        // If RPC response contains term T > currentTerm: set currentTerm = T, convert to follower
        if result.term > self.current_term {
            self.current_term = result.term;
            self.voted_for = None;
            self.state = RaftState::Follower;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_node() {
        let node = RaftNode::new(1, vec![2, 3]);
        assert_eq!(node.id, 1);
        assert_eq!(node.current_term, 0);
        assert_eq!(node.state, RaftState::Follower);
        assert_eq!(node.log.len(), 0);
    }

    #[test]
    fn test_election() {
        let mut node = RaftNode::new(1, vec![2, 3]);
        node.start_election();
        assert_eq!(node.state, RaftState::Candidate);
        assert_eq!(node.current_term, 1);
        assert_eq!(node.voted_for, Some(1));
    }

    #[test]
    fn test_request_vote() {
        let mut node = RaftNode::new(1, vec![2, 3]);
        let args = RequestVoteArgs {
            term: 1,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        };
        let result = node.handle_request_vote(args);
        assert!(result.vote_granted);
        assert_eq!(node.voted_for, Some(2));
    }

    #[test]
    fn test_append_entries() {
        let mut node = RaftNode::new(1, vec![2, 3]);
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
        let result = node.handle_append_entries(args);
        assert!(result.success);
        assert_eq!(node.log.len(), 1);
        assert_eq!(node.state, RaftState::Follower);
    }
}

