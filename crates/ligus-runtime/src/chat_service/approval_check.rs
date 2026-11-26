//! Check 5 of code-match approval on the service's folded state
//! ([`crate::approval_mailbox::LogApprovalCheck`]).

use ligus_core::{ActorKind, SessionEvent, WorkspaceRole};

use super::{ChatError, ChatService};
use crate::approval_mailbox::{LogApproval, LogApprovalCheck, LogSubject, LogVerdict};

fn not_yet(why: &str) -> LogVerdict {
    LogVerdict::NotYet(why.to_string())
}

fn refused(why: &str) -> LogVerdict {
    LogVerdict::Refused(why.to_string())
}

impl LogApprovalCheck for ChatService {
    fn check_approval_on_log(&self, a: &LogApproval<'_>) -> LogVerdict {
        match self.approval_on_log(a) {
            Ok(v) => v,
            Err(e) => LogVerdict::NotYet(e.to_string()),
        }
    }
}

impl ChatService {
    fn approval_on_log(&self, a: &LogApproval<'_>) -> Result<LogVerdict, ChatError> {
        let ws = a.workspace_id;
        let Some(pin) = self.store.workspace_pin(ws)? else {
            return Ok(refused("this device has no founder pin for the workspace"));
        };
        let fk: String = pin.fk.iter().map(|b| format!("{b:02x}")).collect();
        if fk != a.founder_pin {
            return Ok(refused("this device pins another founder than the approval address"));
        }
        if self.waiting_for_founder(ws)?.is_some() {
            return Ok(not_yet("the workspace's founding hasn't synced yet"));
        }
        match &a.subject {
            LogSubject::Box { invite_id, device_key } => {
                let (config, timeline) = self.pinned_config(ws)?;
                let Some(config) = config else { return Ok(not_yet("the workspace hasn't synced yet")) };
                let Some(admission) = config.guards.service_invites.get(invite_id) else {
                    return Ok(not_yet("the admission isn't on the workspace log yet"));
                };
                if admission.device_key.as_deref() != Some(*device_key) {
                    return Ok(refused("the admission on the log is bound to another device"));
                }
                let envelopes = self.store.config_envelopes(ws)?;
                let founder = pin.founder_account.to_string();
                let verdicts = ligus_core::fold_copy_decisions(&envelopes, Some(&founder), self.local_workspace_id, ligus_core::WorkspaceAuthz::Unchecked);
                let subject = format!("invite:{invite_id}");
                // The rules allow a box admission only from an Admin, with
                // passkey presence: an allowed one carried both.
                let Some(env) = envelopes.iter().find(|e| {
                    matches!(&e.payload, SessionEvent::JoinApproved { subject: s, admission: Some(_), .. } if *s == subject)
                        && verdicts.get(e) == Some(ligus_core::Verdict::Allowed)
                }) else {
                    return Ok(not_yet("the admission isn't on the workspace log yet"));
                };
                if env.signer_device_id != Some(a.approver_device_id) {
                    return Ok(refused("the admission on the log was signed by another device than the one that showed the code"));
                }
                let head = timeline.current();
                let Some(device) = head.devices.get(&a.approver_device_id) else {
                    return Ok(refused("the approving device isn't certified in this workspace"));
                };
                if device.revoked || device.public_key != a.approver_sig_key {
                    return Ok(refused("the approving device is revoked or isn't the one that showed the code"));
                }
                let Some(account) = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) else {
                    return Ok(refused("the admission names no account"));
                };
                if !device.accounts.contains(&account) {
                    return Ok(refused("the approving device isn't certified for the account that signed"));
                }
                let Some(member) = config.members.iter().find(|m| m.actor.kind == ActorKind::Human && m.actor.account_id == Some(account)) else {
                    return Ok(refused("the approver isn't a member of this workspace"));
                };
                if config.role_of(&member.actor.id).rank() < WorkspaceRole::Admin.rank() {
                    return Ok(refused("the approver isn't an Admin of this workspace"));
                }
                Ok(LogVerdict::Verified { account_id: account, name: crate::display::shown(&member.actor.display_name) })
            }
            LogSubject::Computer { grant } => {
                if grant.approver_key != a.approver_sig_key || grant.approver_device_id != a.approver_device_id {
                    return Ok(refused("the grant wasn't issued by the device that showed the code"));
                }
                if grant.workspace_id != ws {
                    return Ok(refused("the grant is for another workspace"));
                }
                match self.check_computer_link(grant, a.now_ms) {
                    Ok(person) => Ok(LogVerdict::Verified {
                        account_id: person.account_id.unwrap_or_default(),
                        name: crate::display::shown(&person.display_name),
                    }),
                    Err(ChatError::Unauthorized(why)) if why == crate::computer_link::NOT_ON_RELAY_YET || why.contains("hasn't synced") => {
                        Ok(LogVerdict::NotYet(why))
                    }
                    Err(ChatError::Unauthorized(why)) => Ok(LogVerdict::Refused(why)),
                    Err(e) => Ok(LogVerdict::NotYet(e.to_string())),
                }
            }
        }
    }
}

impl ChatService {
    /// The workspace keys this device holds for code-match approval, one per
    /// authorized epoch (the passphrase key as epoch 0 before any rotation),
    /// for [`crate::approval_mailbox::list_live`] and
    /// [`crate::approval_mailbox::current_address`].
    pub fn approval_epoch_keys(
        &self,
        workspace_id: uuid::Uuid,
        src: &crate::keyring::KeySource,
    ) -> Result<Vec<crate::approval_mailbox::EpochKey>, ChatError> {
        let (_, timeline) = self.pinned_config(workspace_id)?;
        let ring = crate::keyring::KeyringEngine::build(workspace_id, Some(&timeline), src, &[]);
        let mut out: Vec<crate::approval_mailbox::EpochKey> =
            ring.authorized_keys(&timeline).into_iter().map(|(epoch, key)| crate::approval_mailbox::EpochKey { epoch, key }).collect();
        if let Some(seal) = ring.seal_key() {
            if !out.iter().any(|e| e.epoch == seal.version) {
                out.push(crate::approval_mailbox::EpochKey { epoch: seal.version, key: seal.key });
            }
        }
        out.sort_by_key(|e| e.epoch);
        Ok(out)
    }

    /// The founder pin this device holds for `workspace_id`, as an approval
    /// address carries it (32 lowercase hex).
    pub fn founder_pin_hex(&self, workspace_id: uuid::Uuid) -> Result<Option<String>, ChatError> {
        Ok(self.store.workspace_pin(workspace_id)?.map(|p| p.fk.iter().map(|b| format!("{b:02x}")).collect()))
    }
}

/// Who can approve what in one workspace, from this device's folded config
/// log: the [`crate::approval_mailbox::ApproverClassifier`] the contested
/// sheet asks (design note 6.4).
#[derive(Debug, Clone, Default)]
pub struct WorkspaceApprovers {
    devices: std::collections::HashMap<uuid::Uuid, ligus_core::authz::DeviceState>,
    admins: std::collections::HashSet<uuid::Uuid>,
    humans: std::collections::HashSet<uuid::Uuid>,
    /// The account whose sheet asks: a computer or a phone request is that
    /// person's own, so only their own devices can approve it.
    viewer: Option<uuid::Uuid>,
}

impl WorkspaceApprovers {
    /// The same roster, read for `account`'s sheet.
    pub fn for_viewer(mut self, account: Option<uuid::Uuid>) -> Self {
        self.viewer = account;
        self
    }
}

impl crate::approval_mailbox::ApproverClassifier for WorkspaceApprovers {
    fn classify(&self, kind: ligus_core::match_code::MatchKind, device_id: uuid::Uuid, key: &[u8]) -> crate::approval_mailbox::Approvability {
        use crate::approval_mailbox::Approvability;
        use ligus_core::match_code::MatchKind;
        let Some(d) = self.devices.get(&device_id) else { return Approvability::Unknown };
        if d.revoked || d.public_key != key {
            return Approvability::Cannot;
        }
        let ok = match kind {
            // An Admin's device (a phone included).
            MatchKind::Box => d.accounts.iter().any(|a| self.admins.contains(a)),
            // The viewer's own device holding their account key: another
            // member's desktop can't add a device of the viewer's.
            MatchKind::Computer | MatchKind::Phone => {
                !d.phone && self.viewer.is_some_and(|v| self.humans.contains(&v) && d.accounts.contains(&v) && !d.delegated_for(v))
            }
        };
        if ok { Approvability::CanApprove } else { Approvability::Cannot }
    }
}

impl ChatService {
    /// [`WorkspaceApprovers`] for `workspace_id`, read for this device's own
    /// account (empty before it syncs: every device then reads as unknown).
    pub fn approval_classifier(&self, workspace_id: uuid::Uuid) -> Result<WorkspaceApprovers, ChatError> {
        let (config, timeline) = self.pinned_config(workspace_id)?;
        let Some(config) = config else { return Ok(WorkspaceApprovers::default()) };
        let head = timeline.current();
        let mut out = WorkspaceApprovers {
            devices: head.devices.iter().map(|(k, v)| (*k, v.clone())).collect(),
            viewer: self.author.account_id,
            ..Default::default()
        };
        for m in config.members.iter().filter(|m| m.actor.kind == ActorKind::Human) {
            let Some(a) = m.actor.account_id else { continue };
            out.humans.insert(a);
            if config.role_of(&m.actor.id).rank() >= WorkspaceRole::Admin.rank() {
                out.admins.insert(a);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;
    use crate::approval_mailbox::{Approvability, ApproverClassifier};
    use ligus_core::match_code::MatchKind;

    /// Another member's desktop can't approve my computer or phone: only my
    /// own devices can (design 6.4, the contested sheet).
    #[test]
    fn only_my_own_devices_can_approve_my_device_requests() {
        let ana = uuid::Uuid::new_v4();
        let mallory = uuid::Uuid::new_v4();
        let (mallory_dev, ana_dev) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let key = vec![7u8; 32];
        let mut w = WorkspaceApprovers::default();
        w.humans.insert(ana);
        w.humans.insert(mallory);
        w.admins.insert(mallory);
        w.devices.insert(mallory_dev, ligus_core::authz::DeviceState { accounts: [mallory].into_iter().collect(), public_key: key.clone(), ..Default::default() });
        w.devices.insert(ana_dev, ligus_core::authz::DeviceState { accounts: [ana].into_iter().collect(), public_key: key.clone(), ..Default::default() });
        let w = w.for_viewer(Some(ana));
        assert_eq!(w.classify(MatchKind::Computer, mallory_dev, &key), Approvability::Cannot);
        assert_eq!(w.classify(MatchKind::Phone, mallory_dev, &key), Approvability::Cannot);
        assert_eq!(w.classify(MatchKind::Computer, ana_dev, &key), Approvability::CanApprove);
        // A box: any Admin's device.
        assert_eq!(w.classify(MatchKind::Box, mallory_dev, &key), Approvability::CanApprove);
        assert_eq!(w.classify(MatchKind::Box, ana_dev, &key), Approvability::Cannot);
        assert_eq!(w.classify(MatchKind::Box, mallory_dev, &[8; 32]), Approvability::Cannot, "another key");
    }
}
