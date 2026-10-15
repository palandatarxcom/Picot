// ABOUTME: Owns host-assigned logical operations independently from runtime instances.
// ABOUTME: Provides scoped idempotency, deterministic expiry, eviction, and invalidation.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationScope {
    pub owner_id: String,
    pub workspace_id: String,
    pub session_id: String,
    pub workspace_generation: u64,
}

impl OperationScope {
    pub fn new(
        owner_id: impl Into<String>,
        workspace_id: impl Into<String>,
        session_id: impl Into<String>,
        workspace_generation: u64,
    ) -> Self {
        Self {
            owner_id: owner_id.into(),
            workspace_id: workspace_id.into(),
            session_id: session_id.into(),
            workspace_generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum OperationState {
    Pending,
    Completed,
    Indeterminate,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OperationRecord {
    pub operation_id: String,
    pub idempotency_key: String,
    pub command_type: String,
    pub scope: OperationScope,
    pub execution_instance_id: String,
    pub accepted_at: Instant,
    pub expires_at: Instant,
    pub state: OperationState,
    pub terminal_response: Option<Value>,
    pub crash_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationLookupError {
    NotFound,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAcceptance {
    Accepted,
    DuplicatePending,
    DuplicateCompleted,
}

/// In-memory logical operation authority. Runtime instance replacement does not remove records.
pub struct OperationRegistry {
    operations: HashMap<String, OperationRecord>,
    order: VecDeque<String>,
    capacity: usize,
    ttl: Duration,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
}

impl OperationRegistry {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self::with_clock(capacity, ttl, Box::new(Instant::now))
    }

    pub fn with_clock(
        capacity: usize,
        ttl: Duration,
        clock: Box<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            operations: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
            ttl,
            clock,
        }
    }

    pub fn accept(
        &mut self,
        scope: OperationScope,
        idempotency_key: impl Into<String>,
        command_type: impl Into<String>,
        execution_instance_id: impl Into<String>,
    ) -> Result<(String, OperationAcceptance), OperationLookupError> {
        let key = idempotency_key.into();
        if key.is_empty() {
            return Err(OperationLookupError::NotFound);
        }
        let now = (self.clock)();
        self.expire(now);
        if let Some(operation_id) = self.find_key(&scope, &key) {
            let stored = self.operations.get(&operation_id).unwrap();
            return match stored.state {
                OperationState::Pending => {
                    Ok((operation_id, OperationAcceptance::DuplicatePending))
                }
                OperationState::Completed => {
                    Ok((operation_id, OperationAcceptance::DuplicateCompleted))
                }
                OperationState::Expired => Err(OperationLookupError::Expired),
                OperationState::Revoked => Err(OperationLookupError::Revoked),
                OperationState::Indeterminate => {
                    Ok((operation_id, OperationAcceptance::DuplicatePending))
                }
            };
        }

        let operation_id = format!("op-{}", Uuid::new_v4());
        self.operations.insert(
            operation_id.clone(),
            OperationRecord {
                operation_id: operation_id.clone(),
                idempotency_key: key,
                command_type: command_type.into(),
                scope,
                execution_instance_id: execution_instance_id.into(),
                accepted_at: now,
                expires_at: now + self.ttl,
                state: OperationState::Pending,
                terminal_response: None,
                crash_reason: None,
            },
        );
        self.order.push_back(operation_id.clone());
        self.evict_if_needed();
        Ok((operation_id, OperationAcceptance::Accepted))
    }

    pub fn get(&mut self, operation_id: &str) -> Result<&OperationRecord, OperationLookupError> {
        self.expire((self.clock)());
        let stored = self
            .operations
            .get(operation_id)
            .ok_or(OperationLookupError::NotFound)?;
        match stored.state {
            OperationState::Expired => Err(OperationLookupError::Expired),
            OperationState::Revoked => Err(OperationLookupError::Revoked),
            _ => Ok(stored),
        }
    }

    /// Lookup operation only when caller presents exact logical scope.
    /// Owner/workspace/session/generation are supplied by the host adapter; this
    /// registry never derives authority from browser paths or runtime targets.
    pub fn get_scoped(
        &mut self,
        operation_id: &str,
        scope: &OperationScope,
    ) -> Result<&OperationRecord, OperationLookupError> {
        let record = self.get(operation_id)?;
        if record.scope != *scope {
            return Err(OperationLookupError::NotFound);
        }
        Ok(record)
    }

    /// Lookup status while enforcing owner/workspace/generation scope. Session
    /// identity remains part of stored scope but is not client supplied for
    /// operation recovery.
    pub fn get_owner_scoped(
        &mut self,
        operation_id: &str,
        owner_id: &str,
        workspace_id: &str,
        generation: u64,
    ) -> Result<&OperationRecord, OperationLookupError> {
        let record = self.get(operation_id)?;
        if record.scope.owner_id != owner_id
            || record.scope.workspace_id != workspace_id
            || record.scope.workspace_generation != generation
        {
            return Err(OperationLookupError::NotFound);
        }
        Ok(record)
    }

    pub fn complete(
        &mut self,
        operation_id: &str,
        response: Value,
    ) -> Result<(), OperationLookupError> {
        let stored = self.mutable(operation_id)?;
        if stored.state == OperationState::Pending {
            stored.state = OperationState::Completed;
            stored.terminal_response = Some(response);
        }
        Ok(())
    }

    pub fn mark_indeterminate(
        &mut self,
        operation_id: &str,
        crash_reason: impl Into<String>,
    ) -> Result<(), OperationLookupError> {
        let stored = self.mutable(operation_id)?;
        if stored.state == OperationState::Pending {
            stored.state = OperationState::Indeterminate;
            stored.crash_reason = Some(crash_reason.into());
        }
        Ok(())
    }

    /// Marks pending operations indeterminate; completed operations remain replayable.
    pub fn instance_replaced(&mut self, execution_instance_id: &str, reason: impl Into<String>) {
        let reason = reason.into();
        self.expire((self.clock)());
        for stored in self.operations.values_mut() {
            if stored.execution_instance_id == execution_instance_id
                && stored.state == OperationState::Pending
            {
                stored.state = OperationState::Indeterminate;
                stored.crash_reason = Some(reason.clone());
            }
        }
    }

    /// Test-only observation of per-instance operation states, so crash
    /// semantics (Pending → Indeterminate with crash reason) can be asserted
    /// directly instead of inferred from opaque operation ids.
    #[cfg(test)]
    pub(crate) fn states_for_instance(
        &self,
        execution_instance_id: &str,
    ) -> Vec<(String, OperationState, Option<String>)> {
        self.operations
            .values()
            .filter(|stored| stored.execution_instance_id == execution_instance_id)
            .map(|stored| {
                (
                    stored.operation_id.clone(),
                    stored.state.clone(),
                    stored.crash_reason.clone(),
                )
            })
            .collect()
    }

    pub fn host_restart(&mut self, crash_reason: impl Into<String>) {
        let reason = crash_reason.into();
        self.expire((self.clock)());
        for stored in self.operations.values_mut() {
            if stored.state == OperationState::Pending {
                stored.state = OperationState::Indeterminate;
                stored.crash_reason = Some(reason.clone());
            }
        }
    }

    pub fn revoke_owner(&mut self, owner_id: &str) {
        self.expire((self.clock)());
        for stored in self.operations.values_mut() {
            if stored.scope.owner_id == owner_id {
                stored.state = OperationState::Revoked;
                stored.terminal_response = None;
            }
        }
    }

    pub fn invalidate_generation(&mut self, owner_id: &str, workspace_id: &str, generation: u64) {
        self.expire((self.clock)());
        for stored in self.operations.values_mut() {
            let scope = &stored.scope;
            if scope.owner_id == owner_id
                && scope.workspace_id == workspace_id
                && scope.workspace_generation != generation
            {
                stored.state = OperationState::Revoked;
                stored.terminal_response = None;
            }
        }
    }

    fn mutable(
        &mut self,
        operation_id: &str,
    ) -> Result<&mut OperationRecord, OperationLookupError> {
        self.expire((self.clock)());
        let stored = self
            .operations
            .get_mut(operation_id)
            .ok_or(OperationLookupError::NotFound)?;
        match stored.state {
            OperationState::Expired => Err(OperationLookupError::Expired),
            OperationState::Revoked => Err(OperationLookupError::Revoked),
            _ => Ok(stored),
        }
    }

    fn find_key(&self, scope: &OperationScope, key: &str) -> Option<String> {
        self.order.iter().find_map(|id| {
            let record = self.operations.get(id)?;
            (record.scope == *scope && record.idempotency_key == key).then(|| id.clone())
        })
    }

    fn expire(&mut self, now: Instant) {
        for stored in self.operations.values_mut() {
            if stored.state != OperationState::Expired && now >= stored.expires_at {
                stored.state = OperationState::Expired;
                stored.terminal_response = None;
            }
        }
    }

    fn evict_if_needed(&mut self) {
        while self.operations.len() > self.capacity {
            let id = self
                .order
                .pop_front()
                .expect("operation order must match records");
            self.operations.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn clock() -> (Arc<Mutex<Instant>>, Box<dyn Fn() -> Instant + Send + Sync>) {
        let now = Arc::new(Mutex::new(Instant::now()));
        let source = Arc::clone(&now);
        (now, Box::new(move || *source.lock().unwrap()))
    }

    fn scope(generation: u64) -> OperationScope {
        OperationScope::new("owner", "workspace", "session", generation)
    }

    #[test]
    fn scope_is_logical_and_generation_sensitive() {
        assert_ne!(scope(1), scope(2));
        assert_eq!(
            scope(1),
            OperationScope::new("owner", "workspace", "session", 1)
        );
    }

    #[test]
    fn ttl_expiry_is_deterministic_and_blocks_replay() {
        let (now, clock) = clock();
        let mut registry = OperationRegistry::with_clock(4, Duration::from_secs(10), clock);
        let (id, _) = registry
            .accept(scope(1), "key", "prompt", "instance")
            .unwrap();
        let record = registry.get(&id).unwrap();
        assert_eq!(
            record.expires_at,
            record.accepted_at + Duration::from_secs(10)
        );
        *now.lock().unwrap() += Duration::from_secs(10);
        assert_eq!(registry.get(&id), Err(OperationLookupError::Expired));
        assert_eq!(
            registry.accept(scope(1), "key", "prompt", "instance"),
            Err(OperationLookupError::Expired)
        );
    }

    #[test]
    fn capacity_evicts_oldest_in_acceptance_order() {
        let (_, clock) = clock();
        let mut registry = OperationRegistry::with_clock(2, Duration::from_secs(60), clock);
        let (first, _) = registry.accept(scope(1), "a", "prompt", "i").unwrap();
        let (second, _) = registry.accept(scope(1), "b", "prompt", "i").unwrap();
        let (third, _) = registry.accept(scope(1), "c", "prompt", "i").unwrap();
        assert!(registry.get(&first).is_err());
        assert_eq!(registry.get(&second).unwrap().idempotency_key, "b");
        assert_eq!(registry.get(&third).unwrap().idempotency_key, "c");
    }

    #[test]
    fn revoke_owner_and_generation_change_hide_terminal_response() {
        let (_, clock) = clock();
        let mut registry = OperationRegistry::with_clock(4, Duration::from_secs(60), clock);
        let (owner_id, _) = registry.accept(scope(1), "a", "prompt", "i").unwrap();
        registry
            .complete(&owner_id, serde_json::json!({"ok": true}))
            .unwrap();
        registry.revoke_owner("owner");
        assert_eq!(registry.get(&owner_id), Err(OperationLookupError::Revoked));

        let (generation_id, _) = registry.accept(scope(2), "b", "prompt", "i").unwrap();
        registry
            .complete(&generation_id, serde_json::json!({"secret": true}))
            .unwrap();
        registry.invalidate_generation("owner", "workspace", 3);
        assert_eq!(
            registry.get(&generation_id),
            Err(OperationLookupError::Revoked)
        );
    }

    #[test]
    fn replay_reports_each_acceptance_state_and_preserves_operation_id() {
        let (_, clock) = clock();
        let mut registry = OperationRegistry::with_clock(4, Duration::from_secs(60), clock);
        let (id, acceptance) = registry
            .accept(scope(1), "same", "prompt", "instance")
            .unwrap();
        assert_eq!(acceptance, OperationAcceptance::Accepted);
        let (pending_id, acceptance) = registry
            .accept(scope(1), "same", "prompt", "other")
            .unwrap();
        assert_eq!(pending_id, id);
        assert_eq!(acceptance, OperationAcceptance::DuplicatePending);
        registry
            .complete(&id, serde_json::json!({"ok": true}))
            .unwrap();
        let (completed_id, acceptance) = registry
            .accept(scope(1), "same", "prompt", "other")
            .unwrap();
        assert_eq!(completed_id, id);
        assert_eq!(acceptance, OperationAcceptance::DuplicateCompleted);
        assert_eq!(
            registry.get(&id).unwrap().terminal_response,
            Some(serde_json::json!({"ok": true}))
        );
    }

    #[test]
    fn replay_is_scoped_and_host_restart_makes_pending_indeterminate() {
        let (_, clock) = clock();
        let mut registry = OperationRegistry::with_clock(4, Duration::from_secs(60), clock);
        let (id, acceptance) = registry.accept(scope(1), "same", "prompt", "old").unwrap();
        assert_eq!(acceptance, OperationAcceptance::Accepted);
        assert_eq!(
            registry
                .accept(scope(1), "same", "prompt", "new")
                .unwrap()
                .1,
            OperationAcceptance::DuplicatePending
        );
        assert_eq!(
            registry
                .accept(scope(2), "same", "prompt", "new")
                .unwrap()
                .1,
            OperationAcceptance::Accepted
        );
        registry.host_restart("runtime_crashed");
        assert_eq!(
            registry.get(&id).unwrap().state,
            OperationState::Indeterminate
        );
        assert_eq!(
            registry.get(&id).unwrap().crash_reason.as_deref(),
            Some("runtime_crashed")
        );
    }
}
