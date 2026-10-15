#![cfg_attr(not(test), allow(dead_code))]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTarget {
    pub workspace_id: String,
    pub session_id: String,
    pub instance_id: String,
    /// Host-derived owner binding. None is retained only for legacy/unit
    /// fixtures; native production admission requires Some.
    pub owner_id: Option<String>,
    /// Wire targets from the browser echo the bootstrap object; partial
    /// triples (pre-bootstrap pages) still deserialize with generation 0 and
    /// are rejected later by authorize_target, which re-reads the registry.
    #[serde(default)]
    pub workspace_generation: u64,
}

impl RuntimeTarget {
    pub fn new(
        workspace_id: impl Into<String>,
        session_id: impl Into<String>,
        instance_id: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            session_id: session_id.into(),
            instance_id: instance_id.into(),
            owner_id: None,
            workspace_generation: 0,
        }
    }

    pub fn with_owner(
        workspace_id: impl Into<String>,
        session_id: impl Into<String>,
        instance_id: impl Into<String>,
        owner_id: impl Into<String>,
        workspace_generation: u64,
    ) -> Self {
        let mut target = Self::new(workspace_id, session_id, instance_id);
        target.owner_id = Some(owner_id.into());
        target.workspace_generation = workspace_generation;
        target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, allow(dead_code))]
pub enum RuntimeState {
    Starting,
    Trusting,
    Ready,
    Working,
    Idle,
    Suspended,
    Crashed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationAcceptance {
    Accepted,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencedEvent {
    pub target: RuntimeTarget,
    pub sequence: u64,
    pub event: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub target: RuntimeTarget,
    pub sequence: u64,
    pub state: RuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinatorError {
    UnknownInstance,
    IdentityMismatch,
    DuplicateSession,
    InvalidState,
    ForbiddenIdentityReplacement,
    InvalidCommand,
    MissingIdempotencyKey,
}

struct RuntimeRecord {
    target: RuntimeTarget,
    state: RuntimeState,
    sequence: u64,
    mutations: VecDeque<MutationRecord>,
}

#[allow(dead_code)]
struct MutationRecord {
    key: String,
    result: Option<Value>,
}

pub struct RuntimeCoordinator {
    instances: HashMap<String, RuntimeRecord>,
    idempotency_capacity: usize,
}

impl RuntimeCoordinator {
    pub fn new(idempotency_capacity: usize) -> Self {
        Self {
            instances: HashMap::new(),
            idempotency_capacity: idempotency_capacity.max(1),
        }
    }

    pub fn register(
        &mut self,
        target: RuntimeTarget,
        state: RuntimeState,
    ) -> Result<(), CoordinatorError> {
        if self.instances.values().any(|record| {
            record.target.workspace_id == target.workspace_id
                && record.target.session_id == target.session_id
        }) {
            return Err(CoordinatorError::DuplicateSession);
        }
        if self.instances.contains_key(&target.instance_id) {
            return Err(CoordinatorError::IdentityMismatch);
        }
        self.instances.insert(
            target.instance_id.clone(),
            RuntimeRecord {
                target,
                state,
                sequence: 0,
                mutations: VecDeque::new(),
            },
        );
        Ok(())
    }

    /// Read-only state probe for summaries: `Working` is the event-driven
    /// streaming signal (agent_start sets it, agent_end/agent_settled clear).
    pub fn state_of(&self, target: &RuntimeTarget) -> Option<RuntimeState> {
        self.instances
            .get(&target.instance_id)
            .map(|record| record.state)
    }

    pub fn validate(&self, target: &RuntimeTarget) -> Result<(), CoordinatorError> {
        let record = self
            .instances
            .get(&target.instance_id)
            .ok_or(CoordinatorError::UnknownInstance)?;
        if record.target != *target {
            // Field-level diff for the mismatch: which identity component
            // drifted is the whole story of every IdentityMismatch report.
            log::error!(
                "[coordinator] identity mismatch: request={:?} registered={:?}",
                target,
                record.target
            );
            return Err(CoordinatorError::IdentityMismatch);
        }
        Ok(())
    }

    pub fn validate_command(
        &self,
        target: &RuntimeTarget,
        command: &Value,
    ) -> Result<(), CoordinatorError> {
        self.validate(target)?;
        let command_type = command
            .get("type")
            .and_then(Value::as_str)
            .ok_or(CoordinatorError::InvalidCommand)?;
        if matches!(command_type, "new_session" | "switch_session") {
            return Err(CoordinatorError::ForbiddenIdentityReplacement);
        }
        Ok(())
    }

    pub fn accept_mutation(
        &mut self,
        target: &RuntimeTarget,
        idempotency_key: &str,
    ) -> Result<MutationAcceptance, CoordinatorError> {
        self.validate(target)?;
        if idempotency_key.is_empty() {
            return Err(CoordinatorError::MissingIdempotencyKey);
        }
        let record = self.instances.get_mut(&target.instance_id).unwrap();
        if record
            .mutations
            .iter()
            .any(|mutation| mutation.key == idempotency_key)
        {
            return Ok(MutationAcceptance::Duplicate);
        }
        record.mutations.push_back(MutationRecord {
            key: idempotency_key.to_owned(),
            result: None,
        });
        while record.mutations.len() > self.idempotency_capacity {
            record.mutations.pop_front();
        }
        Ok(MutationAcceptance::Accepted)
    }

    #[allow(dead_code)]
    pub fn complete_mutation(
        &mut self,
        target: &RuntimeTarget,
        idempotency_key: &str,
        result: Value,
    ) -> Result<(), CoordinatorError> {
        self.validate(target)?;
        let record = self.instances.get_mut(&target.instance_id).unwrap();
        let mutation = record
            .mutations
            .iter_mut()
            .find(|mutation| mutation.key == idempotency_key)
            .ok_or(CoordinatorError::MissingIdempotencyKey)?;
        mutation.result = Some(result);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn mutation_result(
        &self,
        target: &RuntimeTarget,
        idempotency_key: &str,
    ) -> Result<Option<Value>, CoordinatorError> {
        self.validate(target)?;
        Ok(self
            .instances
            .get(&target.instance_id)
            .unwrap()
            .mutations
            .iter()
            .find(|mutation| mutation.key == idempotency_key)
            .and_then(|mutation| mutation.result.clone()))
    }

    pub fn emit_event(
        &mut self,
        target: &RuntimeTarget,
        event: Value,
    ) -> Result<SequencedEvent, CoordinatorError> {
        self.validate(target)?;
        let record = self.instances.get_mut(&target.instance_id).unwrap();
        record.sequence += 1;
        Ok(SequencedEvent {
            target: target.clone(),
            sequence: record.sequence,
            event,
        })
    }

    pub fn snapshot(&self, target: &RuntimeTarget) -> Result<RuntimeSnapshot, CoordinatorError> {
        self.validate(target)?;
        let record = self.instances.get(&target.instance_id).unwrap();
        Ok(RuntimeSnapshot {
            target: record.target.clone(),
            sequence: record.sequence,
            state: record.state,
        })
    }

    pub fn set_state(
        &mut self,
        target: &RuntimeTarget,
        state: RuntimeState,
    ) -> Result<(), CoordinatorError> {
        // State transitions arrive from wire-frame targets (no owner/generation
        // on the wire); identity triple only, same as the turn lifecycle.
        self.validate_identity(target)?;
        let record = self.instances.get_mut(&target.instance_id).unwrap();
        record.state = state;
        Ok(())
    }

    /// Identity-only validation for the abort read path: a client target can
    /// lag a workspace transition (owner binding, generation bump) while
    /// still naming the exact same runtime (workspace + session + instance,
    /// where instance equality is the map lookup). Abort acts on the bound
    /// active turn of that exact instance, so stale routing bookkeeping on
    /// the caller's copy must never block stopping a live run.
    pub(crate) fn validate_identity(&self, target: &RuntimeTarget) -> Result<(), CoordinatorError> {
        let record = self
            .instances
            .get(&target.instance_id)
            .ok_or(CoordinatorError::UnknownInstance)?;
        if record.target.workspace_id != target.workspace_id
            || record.target.session_id != target.session_id
        {
            log::error!(
                "[coordinator] identity mismatch: request={:?} registered={:?}",
                target,
                record.target
            );
            return Err(CoordinatorError::IdentityMismatch);
        }
        Ok(())
    }

    pub fn bind_session_id(
        &mut self,
        temporary: &RuntimeTarget,
        session_id: impl Into<String>,
    ) -> Result<RuntimeTarget, CoordinatorError> {
        self.validate(temporary)?;
        let session_id = session_id.into();
        if session_id.is_empty() {
            return Err(CoordinatorError::IdentityMismatch);
        }
        if self.instances.values().any(|record| {
            record.target.instance_id != temporary.instance_id
                && record.target.workspace_id == temporary.workspace_id
                && record.target.session_id == session_id
        }) {
            return Err(CoordinatorError::DuplicateSession);
        }
        let record = self.instances.get_mut(&temporary.instance_id).unwrap();
        record.target.session_id = session_id;
        Ok(record.target.clone())
    }

    /// Re-stamp one registered runtime's workspace generation (workspace
    /// transitions that reuse a live runtime; the commit sweep stops every
    /// runtime with an older generation, so the reused target must move up).
    pub fn rebind_generation(
        &mut self,
        target: &RuntimeTarget,
        generation: u64,
    ) -> Result<RuntimeTarget, CoordinatorError> {
        self.validate(target)?;
        let record = self.instances.get_mut(&target.instance_id).unwrap();
        record.target.workspace_generation = generation;
        Ok(record.target.clone())
    }

    pub fn unregister(&mut self, target: &RuntimeTarget) -> Result<(), CoordinatorError> {
        self.validate(target)?;
        self.instances.remove(&target.instance_id);
        Ok(())
    }

    pub fn resume(
        &mut self,
        suspended: &RuntimeTarget,
        new_instance_id: impl Into<String>,
    ) -> Result<RuntimeTarget, CoordinatorError> {
        self.validate(suspended)?;
        let record = self.instances.get(&suspended.instance_id).unwrap();
        if record.state != RuntimeState::Suspended {
            return Err(CoordinatorError::InvalidState);
        }
        let new_target = RuntimeTarget::new(
            &suspended.workspace_id,
            &suspended.session_id,
            new_instance_id,
        );
        if self.instances.contains_key(&new_target.instance_id) {
            return Err(CoordinatorError::IdentityMismatch);
        }
        self.instances.remove(&suspended.instance_id);
        self.register(new_target.clone(), RuntimeState::Starting)?;
        Ok(new_target)
    }
}

#[cfg(test)]
mod tests {
    use super::{MutationAcceptance, RuntimeCoordinator, RuntimeState, RuntimeTarget};
    use serde_json::json;

    fn target(instance: &str) -> RuntimeTarget {
        RuntimeTarget::new("workspace-a", "session-a", instance)
    }

    #[test]
    fn wire_target_without_generation_deserializes_to_zero() {
        // Browser frames echo the bootstrap target; a partial pre-bootstrap
        // triple must deserialize instead of failing with a misleading
        // "Runtime target is invalid". Admission still rejects it later.
        let parsed: RuntimeTarget = serde_json::from_value(json!({
            "workspaceId": "workspace-a",
            "sessionId": "session-a",
            "instanceId": "instance-a"
        }))
        .unwrap();
        assert_eq!(parsed.workspace_generation, 0);
        assert!(parsed.owner_id.is_none());
    }

    #[test]
    fn abort_read_path_tolerates_stale_transition_metadata() {
        // Regression: after a workspace transition the registered record
        // carries the owner binding and bumped generation, while a client
        // that missed the re-stamp still sends owner=None/generation=0.
        // State transitions (agent_start/agent_end from the wire target) and
        // the abort path must match on the identity triple only — stopping a
        // live run must not be blocked by routing bookkeeping.
        let mut coordinator = RuntimeCoordinator::new(8);
        let registered =
            RuntimeTarget::with_owner("workspace-a", "session-a", "instance-a", "owner-1", 1);
        coordinator
            .register(registered, RuntimeState::Ready)
            .unwrap();

        let stale_metadata = target("instance-a");
        assert_eq!(stale_metadata.owner_id, None);
        assert_eq!(stale_metadata.workspace_generation, 0);
        // State arrives on the wire target (stale metadata): it must land.
        coordinator
            .set_state(&stale_metadata, RuntimeState::Working)
            .unwrap();
        assert_eq!(
            coordinator.state_of(&stale_metadata),
            Some(RuntimeState::Working)
        );
        // Strict validate keeps guarding the rest: metadata drift still
        // rejects there, and identity drift rejects everywhere.
        assert!(coordinator.validate(&stale_metadata).is_err());
        assert!(coordinator
            .set_state(
                &RuntimeTarget::new("workspace-a", "session-b", "instance-a"),
                RuntimeState::Idle
            )
            .is_err());
    }

    #[test]
    fn validates_all_target_identities_and_rejects_session_replacement() {
        let mut coordinator = RuntimeCoordinator::new(8);
        coordinator
            .register(target("instance-a"), RuntimeState::Ready)
            .unwrap();

        assert!(coordinator.validate(&target("instance-a")).is_ok());
        assert!(coordinator
            .validate(&RuntimeTarget::new(
                "workspace-b",
                "session-a",
                "instance-a"
            ))
            .is_err());
        assert!(coordinator
            .validate(&RuntimeTarget::new(
                "workspace-a",
                "session-b",
                "instance-a"
            ))
            .is_err());
        assert!(coordinator.validate(&target("stale-instance")).is_err());
        assert!(coordinator
            .validate_command(&target("instance-a"), &json!({ "type": "new_session" }))
            .is_err());
        assert!(coordinator
            .validate_command(
                &target("instance-a"),
                &json!({ "type": "switch_session", "sessionPath": "/tmp/session.jsonl" }),
            )
            .is_err());
    }

    #[test]
    fn deduplicates_mutations_and_sequences_events_per_instance() {
        let mut coordinator = RuntimeCoordinator::new(2);
        coordinator
            .register(target("instance-a"), RuntimeState::Ready)
            .unwrap();

        assert_eq!(
            coordinator
                .accept_mutation(&target("instance-a"), "intent-1")
                .unwrap(),
            MutationAcceptance::Accepted
        );
        assert_eq!(
            coordinator
                .accept_mutation(&target("instance-a"), "intent-1")
                .unwrap(),
            MutationAcceptance::Duplicate
        );
        coordinator
            .accept_mutation(&target("instance-a"), "intent-2")
            .unwrap();
        coordinator
            .accept_mutation(&target("instance-a"), "intent-3")
            .unwrap();
        assert_eq!(
            coordinator
                .accept_mutation(&target("instance-a"), "intent-1")
                .unwrap(),
            MutationAcceptance::Accepted,
            "bounded cache evicts the least-recent accepted key"
        );

        let first = coordinator
            .emit_event(&target("instance-a"), json!({ "type": "agent_start" }))
            .unwrap();
        let second = coordinator
            .emit_event(&target("instance-a"), json!({ "type": "agent_end" }))
            .unwrap();
        assert_eq!((first.sequence, second.sequence), (1, 2));
        assert_eq!(
            coordinator
                .snapshot(&target("instance-a"))
                .unwrap()
                .sequence,
            2
        );
    }

    #[test]
    fn resume_preserves_session_but_replaces_instance() {
        let mut coordinator = RuntimeCoordinator::new(8);
        coordinator
            .register(target("instance-a"), RuntimeState::Suspended)
            .unwrap();
        let resumed = coordinator
            .resume(&target("instance-a"), "instance-b")
            .unwrap();

        assert_eq!(resumed.workspace_id, "workspace-a");
        assert_eq!(resumed.session_id, "session-a");
        assert_eq!(resumed.instance_id, "instance-b");
        assert!(coordinator.validate(&target("instance-a")).is_err());
        assert_eq!(
            coordinator.snapshot(&resumed).unwrap().state,
            RuntimeState::Starting
        );
    }

    #[test]
    fn bare_abort_passes_command_validation() {
        // Pi's RPC abort command legitimately carries no turnId
        // (rpc-commands.md); the generic request path (Quick/Side Chat
        // forward_command) must not reject it before it reaches the runtime.
        let mut coordinator = RuntimeCoordinator::new(8);
        let active = target("instance-a");
        coordinator
            .register(active.clone(), RuntimeState::Working)
            .unwrap();
        assert!(coordinator
            .validate_command(&active, &json!({ "type": "abort" }))
            .is_ok());
    }

    #[test]
    fn binds_a_formal_session_without_replacing_the_instance() {
        let temporary = RuntimeTarget::new("workspace-a", "temporary-a", "instance-a");
        let mut coordinator = RuntimeCoordinator::new(8);
        coordinator
            .register(temporary.clone(), RuntimeState::Ready)
            .unwrap();

        let formal = coordinator
            .bind_session_id(&temporary, "session-formal")
            .unwrap();

        assert_eq!(formal.instance_id, temporary.instance_id);
        assert_eq!(formal.session_id, "session-formal");
        assert!(coordinator.validate(&temporary).is_err());
        assert!(coordinator.validate(&formal).is_ok());
    }
}
