// ABOUTME: Host-side OAuth operation lifecycle and generation authority.
// ABOUTME: Pi remains credential owner; host stores only redacted operation state.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthClient {
    Desktop,
    Remote,
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthError {
    DesktopOwnerRequired,
    StaleGeneration,
    OperationNotFound,
    OperationLimit,
}
impl OAuthError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::DesktopOwnerRequired => "desktop_owner_required",
            Self::StaleGeneration => "stale_generation",
            Self::OperationNotFound => "oauth_operation_not_found",
            Self::OperationLimit => "oauth_operation_limit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthStatus {
    Pending,
    /// Pi device-code bridge and MCP login runner can complete operations.
    Succeeded,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthOperation {
    pub id: String,
    pub owner_id: String,
    pub generation: u64,
    pub status: OAuthStatus,
    pub expires_at: Instant,
}

#[derive(Debug, Default)]
pub struct OAuthManager {
    generation: u64,
    operations: HashMap<String, OAuthOperation>,
}

/// Upper bound on tracked operations. Without it a desktop client could
/// enqueue unbounded distinct operation ids that never expire-sweep
/// because nothing polls them.
const MAX_OPERATIONS: usize = 32;

impl OAuthManager {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn runtime_started(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.revoke_prior();
        self.generation
    }
    pub fn runtime_stopped(&mut self) {
        self.revoke_prior();
    }
    pub fn start(
        &mut self,
        client: OAuthClient,
        owner_id: &str,
        id: impl Into<String>,
        ttl: Duration,
    ) -> Result<OAuthOperation, OAuthError> {
        if client != OAuthClient::Desktop {
            return Err(OAuthError::DesktopOwnerRequired);
        }
        let now = Instant::now();
        // Expired-but-unpolled operations must not linger: sweep on every
        // start so unpolled ids cannot accumulate for the process lifetime.
        self.operations
            .retain(|_, operation| now < operation.expires_at);
        if self.operations.len() >= MAX_OPERATIONS {
            return Err(OAuthError::OperationLimit);
        }
        let operation = OAuthOperation {
            id: id.into(),
            owner_id: owner_id.into(),
            generation: self.generation,
            status: OAuthStatus::Pending,
            expires_at: now + ttl,
        };
        self.operations
            .insert(operation.id.clone(), operation.clone());
        Ok(operation)
    }
    pub fn status(
        &mut self,
        owner_id: &str,
        generation: u64,
        id: &str,
    ) -> Result<OAuthStatus, OAuthError> {
        let operation = self
            .operations
            .get(id)
            .ok_or(OAuthError::OperationNotFound)?;
        if operation.owner_id != owner_id {
            // Cross-owner lookups must not reveal operation existence.
            return Err(OAuthError::OperationNotFound);
        }
        if operation.generation != generation || generation != self.generation {
            return Err(OAuthError::StaleGeneration);
        }
        if Instant::now() >= operation.expires_at {
            self.operations.remove(id);
            return Ok(OAuthStatus::Failed);
        }
        Ok(operation.status.clone())
    }
    /// Complete a pending operation only for its original desktop owner and generation.
    pub fn complete(
        &mut self,
        owner_id: &str,
        generation: u64,
        id: &str,
        outcome: OAuthOutcome,
    ) -> Result<(), OAuthError> {
        let operation = self
            .operations
            .get_mut(id)
            .ok_or(OAuthError::OperationNotFound)?;
        if operation.owner_id != owner_id {
            return Err(OAuthError::OperationNotFound);
        }
        if operation.generation != generation || generation != self.generation {
            return Err(OAuthError::StaleGeneration);
        }
        if Instant::now() >= operation.expires_at {
            self.operations.remove(id);
            return Err(OAuthError::OperationNotFound);
        }
        if matches!(operation.status, OAuthStatus::Pending) {
            operation.status = match outcome {
                OAuthOutcome::Succeeded => OAuthStatus::Succeeded,
                OAuthOutcome::Failed => OAuthStatus::Failed,
            };
        }
        Ok(())
    }
    pub fn cancel(
        &mut self,
        owner_id: &str,
        generation: u64,
        id: &str,
    ) -> Result<OAuthStatus, OAuthError> {
        let operation = self
            .operations
            .get(id)
            .ok_or(OAuthError::OperationNotFound)?;
        if operation.owner_id != owner_id {
            // Match status(): foreign owners must not learn operation existence.
            return Err(OAuthError::OperationNotFound);
        }
        if operation.generation != generation || generation != self.generation {
            return Err(OAuthError::StaleGeneration);
        }
        if Instant::now() >= operation.expires_at {
            // Expiry wins over cancellation. Return the same terminal state as
            // status(), then forget the operation so a second query is absent.
            self.operations.remove(id);
            return Ok(OAuthStatus::Failed);
        }
        self.operations
            .get_mut(id)
            .expect("operation checked above")
            .status = OAuthStatus::Cancelled;
        Ok(OAuthStatus::Cancelled)
    }
    fn revoke_prior(&mut self) {
        self.operations
            .retain(|_, operation| operation.generation == self.generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binds_operation_to_owner_and_generation() {
        let mut manager = OAuthManager::default();
        let generation = manager.runtime_started();
        let operation = manager
            .start(
                OAuthClient::Desktop,
                "owner-a",
                "op",
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(
            manager
                .status("owner-a", generation, &operation.id)
                .unwrap(),
            OAuthStatus::Pending
        );
        // m9: cross-owner lookups must not reveal operation existence.
        assert_eq!(
            manager.status("owner-b", generation, "op"),
            Err(OAuthError::OperationNotFound)
        );
        manager.runtime_started();
        assert_eq!(
            manager.status("owner-a", generation, "op"),
            Err(OAuthError::OperationNotFound)
        );
    }
    #[test]
    fn denies_remote_and_ephemeral_without_creating_state() {
        let mut manager = OAuthManager::default();
        assert_eq!(
            manager.start(OAuthClient::Remote, "owner", "r", Duration::ZERO),
            Err(OAuthError::DesktopOwnerRequired)
        );
        assert_eq!(
            manager.start(OAuthClient::Ephemeral, "owner", "e", Duration::ZERO),
            Err(OAuthError::DesktopOwnerRequired)
        );
        assert_eq!(manager.operations.len(), 0);
    }
    #[test]
    fn cancel_expired_operation_matches_status_expiry_precedence() {
        let mut manager = OAuthManager::default();
        let generation = manager.runtime_started();
        let operation = manager
            .start(OAuthClient::Desktop, "owner", "expired", Duration::ZERO)
            .unwrap();
        assert_eq!(
            manager.cancel("owner", generation, &operation.id),
            Ok(OAuthStatus::Failed)
        );
        assert_eq!(
            manager.status("owner", generation, &operation.id),
            Err(OAuthError::OperationNotFound)
        );
    }

    #[test]
    fn completion_is_owner_and_generation_bound_and_terminal() {
        let mut manager = OAuthManager::default();
        let generation = manager.runtime_started();
        manager
            .start(OAuthClient::Desktop, "owner", "op", Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            manager.complete("other", generation, "op", OAuthOutcome::Succeeded),
            Err(OAuthError::OperationNotFound)
        );
        assert_eq!(
            manager.complete("owner", generation + 1, "op", OAuthOutcome::Succeeded),
            Err(OAuthError::StaleGeneration)
        );
        manager
            .complete("owner", generation, "op", OAuthOutcome::Succeeded)
            .unwrap();
        manager
            .complete("owner", generation, "op", OAuthOutcome::Failed)
            .unwrap();
        assert_eq!(
            manager.status("owner", generation, "op"),
            Ok(OAuthStatus::Succeeded)
        );
        manager
            .start(
                OAuthClient::Desktop,
                "owner",
                "cancelled",
                Duration::from_secs(60),
            )
            .unwrap();
        manager.cancel("owner", generation, "cancelled").unwrap();
        manager
            .complete("owner", generation, "cancelled", OAuthOutcome::Succeeded)
            .unwrap();
        assert_eq!(
            manager.status("owner", generation, "cancelled"),
            Ok(OAuthStatus::Cancelled)
        );
    }
    #[test]
    fn sweeps_expired_on_start_and_enforces_operation_limit() {
        let mut manager = OAuthManager::default();
        manager.runtime_started();
        assert!(manager
            .start(OAuthClient::Desktop, "owner", "expired", Duration::ZERO)
            .is_ok());
        // Expired operations are swept by the next start, not accumulated.
        assert!(manager
            .start(
                OAuthClient::Desktop,
                "owner",
                "fresh",
                Duration::from_secs(60)
            )
            .is_ok());
        assert_eq!(manager.operations.len(), 1);
        for index in 0..MAX_OPERATIONS {
            let id = format!("batch-{index}");
            if manager
                .start(OAuthClient::Desktop, "owner", id, Duration::from_secs(60))
                .is_err()
            {
                break;
            }
        }
        assert_eq!(
            manager.start(
                OAuthClient::Desktop,
                "owner",
                "over-limit",
                Duration::from_secs(60)
            ),
            Err(OAuthError::OperationLimit)
        );
    }
}
