//! Transport abstraction for Raft RPC communication

use async_trait::async_trait;

use crate::core::raft_core::{
    AppendEntriesArgs, AppendEntriesResult, InstallSnapshotArgs, InstallSnapshotResult,
    RequestVoteArgs, RequestVoteResult,
};

/// Transport abstraction for Raft RPC communication
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a RequestVote RPC to a peer node
    async fn request_vote(
        &self,
        target: u64,
        args: RequestVoteArgs,
    ) -> Result<RequestVoteResult, TransportError>;

    /// Send an AppendEntries RPC to a peer node
    async fn append_entries(
        &self,
        target: u64,
        args: AppendEntriesArgs,
    ) -> Result<AppendEntriesResult, TransportError>;

    /// Send an InstallSnapshot RPC to a peer node
    async fn install_snapshot(
        &self,
        target: u64,
        args: InstallSnapshotArgs,
    ) -> Result<InstallSnapshotResult, TransportError>;
}

/// Errors that can occur during transport operations
#[derive(Debug, Clone)]
pub enum TransportError {
    /// Connection to the target node failed
    ConnectionFailed,
    /// Request timed out
    Timeout,
    /// Target node not found
    NodeNotFound,
}
