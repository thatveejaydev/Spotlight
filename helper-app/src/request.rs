//! Commands sent to the Premiere plugin that wait for a reply.

use std::time::{Duration, Instant};

/// How long Spotlight waits for Premiere to answer a command.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(12);

/// A command waiting for the reply from one plugin connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    pub peer_id: usize,
    pub request_id: u64,
    pub deadline: Instant,
}

impl PendingRequest {
    /// Starts waiting now, for up to [`ACTION_TIMEOUT`].
    pub fn new(peer_id: usize, request_id: u64) -> Self {
        Self {
            peer_id,
            request_id,
            deadline: Instant::now() + ACTION_TIMEOUT,
        }
    }

    /// True when the reply from `peer_id` for `request_id` answers this request.
    pub fn is_answered_by(&self, peer_id: usize, request_id: u64) -> bool {
        self.peer_id == peer_id && self.request_id == request_id
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

/// A plugin's `{ ok, message }` reply to a placement, Source Monitor or action command.
#[derive(Debug)]
pub struct CommandReply {
    pub peer_id: usize,
    pub request_id: u64,
    pub ok: bool,
    pub message: String,
}

impl CommandReply {
    pub fn answers(&self, request: &PendingRequest) -> bool {
        request.is_answered_by(self.peer_id, self.request_id)
    }
}
