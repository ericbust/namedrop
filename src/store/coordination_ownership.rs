use super::*;
use crate::CoordinationId;
use serde_json::{Value, json};

pub(super) struct CoordinationOwnerRow {
    pub(super) address: PaneAddressId,
    pub(super) session: crate::AgentSessionId,
    pub(super) outcome: String,
    pub(super) request_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoordinationRole {
    Owner,
    OwnerAddressOtherSession,
    Other,
}

impl CoordinationRole {
    pub(crate) fn of(
        caller: &RoutingAddressSnapshot,
        owner_address: &PaneAddressId,
        owner_session: Option<&crate::AgentSessionId>,
    ) -> Self {
        if caller.address_id != *owner_address {
            Self::Other
        } else if caller.agent_session_id.as_ref() == owner_session {
            Self::Owner
        } else {
            Self::OwnerAddressOtherSession
        }
    }
}

const COORDINATION_OTHER_SESSION: &str = "coordination belongs to another session of this address; the restarted owner session can transfer it with handoff adopt";
const COORDINATION_OTHER_SESSION_RESOLVED: &str =
    "coordination belongs to another session of this address and is already resolved";
const COORDINATION_NOT_OWNED: &str =
    "coordination requires its original workspace, address, and exact owner session";
const ADOPT_OTHER_ADDRESS: &str = "coordination belongs to another address; only the current session of its owner address can adopt it";
const ADOPT_ALREADY_OWNER: &str = "this session already owns the coordination; handoff adopt only takes over from an earlier session of this address";
const ADOPT_REQUEST_ID_REUSED: &str = "request id was already used for another coordination";
const ADOPT_DIFFERENT_REQUEST_ID: &str =
    "coordination was already adopted with a different request id";

fn coordination_ownership_entity(id: &CoordinationId, session: &crate::AgentSessionId) -> String {
    format!("{}:{}", id.as_str(), session.as_str())
}

fn previous_owner_from_entity<'a>(entity: &'a str, id: &CoordinationId) -> Option<&'a str> {
    entity.strip_prefix(id.as_str())?.strip_prefix(':')
}

impl StateTransaction<'_> {
    pub(crate) fn require_coordination_owner(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
        owner: &RoutingAddressSnapshot,
        open: bool,
    ) -> Result<(), StoreError> {
        self.require_coordination_capability(ns, owner)?;
        let Some(row) = self.coordination_owner_row(ns, id)? else {
            return Err(StoreError::invalid_input(COORDINATION_NOT_OWNED));
        };
        match CoordinationRole::of(owner, &row.address, Some(&row.session)) {
            CoordinationRole::Other => Err(StoreError::invalid_input(COORDINATION_NOT_OWNED)),
            CoordinationRole::OwnerAddressOtherSession if row.outcome == "open" => {
                Err(StoreError::invalid_input(COORDINATION_OTHER_SESSION))
            }
            CoordinationRole::OwnerAddressOtherSession => Err(StoreError::invalid_input(
                COORDINATION_OTHER_SESSION_RESOLVED,
            )),
            CoordinationRole::Owner if row.outcome == "open" || !open => Ok(()),
            CoordinationRole::Owner => Err(StoreError::invalid_input(
                "coordination is already resolved",
            )),
        }
    }

    pub(super) fn coordination_owner_row(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
    ) -> Result<Option<CoordinationOwnerRow>, StoreError> {
        self.transaction
            .query_row(
                "SELECT owner_address_id,owner_session_id,outcome,request_id FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",
                params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()],
                |r| {
                    Ok(CoordinationOwnerRow {
                        address: PaneAddressId::new(r.get::<_, String>(0)?),
                        session: crate::AgentSessionId::new(r.get::<_, String>(1)?),
                        outcome: r.get(2)?,
                        request_id: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(crate) fn adopt_coordination(
        &mut self,
        ns: &StateNamespace,
        caller: &RoutingAddressSnapshot,
        input: &crate::coordination::AdoptInput,
        now: u64,
    ) -> Result<Value, StoreError> {
        self.require_coordination_capability(ns, caller)?;
        let session = caller
            .agent_session_id
            .as_ref()
            .expect("capability requires a session");
        let id = &input.coordination_id;
        let row = self.coordination_owner_row(ns, id)?.ok_or_else(|| {
            StoreError::invalid_input("coordination does not exist in this workspace")
        })?;
        match CoordinationRole::of(caller, &row.address, Some(&row.session)) {
            CoordinationRole::Other => Err(StoreError::invalid_input(ADOPT_OTHER_ADDRESS)),
            CoordinationRole::Owner => match self.previous_owner_adopted_from(ns, id, session)? {
                None => Err(StoreError::invalid_input(ADOPT_ALREADY_OWNER)),
                Some(previous) if input.request_id == row.request_id => {
                    self.adoption_report(ns, id, caller, &previous, false)
                }
                Some(_) => Err(StoreError::invalid_input(ADOPT_DIFFERENT_REQUEST_ID)),
            },
            CoordinationRole::OwnerAddressOtherSession => {
                if row.outcome != "open" {
                    return Err(StoreError::invalid_input(
                        "coordination is already resolved",
                    ));
                }
                if self.request_id_keys_coordination(ns, caller, &input.request_id)? {
                    return Err(StoreError::invalid_input(ADOPT_REQUEST_ID_REUSED));
                }
                self.transaction.execute(
                    "UPDATE coordinations SET owner_session_id=?4,request_id=?5,revision=revision+1,updated_at_ms=?6
                     WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",
                    params![ns.session_socket_id,ns.space_id.as_str(),id.as_str(),session.as_str(),input.request_id,to_i64(now)?],
                )?;
                let by = MutationAttribution::new(ActorKind::AgentSession, session.as_str(), now);
                self.append_transition(
                    ns,
                    TransitionEntityKind::CoordinationOwnership,
                    &coordination_ownership_entity(id, &row.session),
                    TransitionKind::Adopted,
                    &by,
                )?;
                self.cancel_queued_assignment_replies(
                    ns,
                    id,
                    &row.address,
                    &row.session,
                    false,
                    &by,
                )?;
                self.resolve_attention_source(
                    ns,
                    crate::AttentionSourceKind::Coordination,
                    id.as_str(),
                    &by,
                )?;
                self.adoption_report(ns, id, caller, &row.session, true)
            }
        }
    }

    fn request_id_keys_coordination(
        &self,
        ns: &StateNamespace,
        caller: &RoutingAddressSnapshot,
        request_id: &str,
    ) -> Result<bool, StoreError> {
        self.transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND owner_address_id=?3 AND owner_session_id=?4 AND request_id=?5)",
                params![ns.session_socket_id,ns.space_id.as_str(),caller.address_id.as_str(),caller.agent_session_id.as_ref().map(|s|s.as_str()),request_id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(StoreError::from)
    }

    pub(super) fn previous_owner_adopted_from(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
        session: &crate::AgentSessionId,
    ) -> Result<Option<crate::AgentSessionId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT entity_id FROM transition_facts WHERE session_socket_id=?1 AND worktree_id=?2 AND entity_kind=?3 AND transition_kind=?4
             AND actor_kind=?5 AND actor_id=?6 ORDER BY worktree_order DESC",
        )?;
        let entities = statement
            .query_map(
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    TransitionEntityKind::CoordinationOwnership.as_str(),
                    TransitionKind::Adopted.as_str(),
                    ActorKind::AgentSession.as_str(),
                    session.as_str()
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entities
            .iter()
            .find_map(|entity| previous_owner_from_entity(entity, id))
            .map(crate::AgentSessionId::new))
    }

    fn adoption_report(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
        caller: &RoutingAddressSnapshot,
        previous: &crate::AgentSessionId,
        changed: bool,
    ) -> Result<Value, StoreError> {
        let mut report = self.coordination_report(ns, id, caller)?;
        report["adoption"] = json!({
            "changed": changed,
            "owner_session_id": report["owner_session_id"].clone(),
            "previous_owner_session_id": previous.as_str(),
        });
        Ok(report)
    }

    pub(crate) fn adoptable_coordinations(
        &self,
        ns: &StateNamespace,
        caller: &RoutingAddressSnapshot,
    ) -> Result<Vec<Value>, StoreError> {
        if self.require_coordination_capability(ns, caller).is_err() {
            return Ok(Vec::new());
        }
        let mut statement = self.transaction.prepare(
            "SELECT coordination_id,deliverable,owner_session_id,revision FROM coordinations
             WHERE session_socket_id=?1 AND worktree_id=?2 AND owner_address_id=?3 AND owner_session_id!=?4 AND outcome='open' ORDER BY rowid DESC",
        )?;
        let rows = statement
            .query_map(
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    caller.address_id.as_str(),
                    caller.agent_session_id.as_ref().map(|s| s.as_str())
                ],
                |r| {
                    Ok(json!({
                        "coordination_id":r.get::<_,String>(0)?,"deliverable":r.get::<_,String>(1)?,
                        "owner_session_id":r.get::<_,String>(2)?,"revision":from_i64(r.get(3)?)?,
                    }))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
