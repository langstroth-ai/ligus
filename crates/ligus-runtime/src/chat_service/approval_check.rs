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
