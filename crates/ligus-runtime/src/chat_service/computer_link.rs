//! "Add a computer" on the service: the desktop's half (issue a grant for a
//! link request) and the new computer's half (adopt the grant's certificate).
//! See [`crate::computer_link`] and `docs/design/link-a-computer.md`.

use ligus_core::crypto::DeviceCertificate;
use ligus_core::{workspace_config_session_id, ActorIdentity, ActorKind, SessionEvent, Timestamp, WorkspaceMember};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};
use crate::computer_link::{Grant, LinkRequest, OpenedGrant, GRANT_TTL_MS};

fn refused(why: impl Into<String>) -> ChatError {
    ChatError::Unauthorized(why.into())
}

/// What the desktop shows before approving a request.
#[derive(Debug, Clone)]
pub struct LinkPreview {
    pub device_id: Uuid,
    pub fingerprint: String,
    pub label: String,
    /// Already one of your devices here (not revoked).
    pub already_mine: bool,
    /// Revoked here: the computer needs a new device key.
    pub revoked: bool,
    /// Already acting here for someone else (their name): the computer
    /// itself signed as them. Refused.
    pub someone_elses: Option<String>,
    /// Members whose account certified this computer's key without it ever
    /// acting for them (their names). A foreign certificate gives them
    /// nothing over it and doesn't stop the link; it is only shown.
    pub also_certified_by: Vec<String>,
    /// The member this computer proved it was here (its account key signed
    /// the request), if any.
    pub previous_member: Option<WorkspaceMember>,
}

impl ChatService {
    /// The desktop's read of `request` for `workspace_id`.
    pub fn preview_computer_link(&self, workspace_id: Uuid, request: &LinkRequest) -> Result<LinkPreview> {
        let (config, timeline) = self.pinned_config(workspace_id)?;
        let head = timeline.current();
        let me = self.author.account_id;
        let device = head.devices.get(&request.device_id);
        let revoked = device.is_some_and(|d| d.revoked);
        let already_mine = device.is_some_and(|d| !d.revoked && me.is_some_and(|a| d.accounts.contains(&a)));
        let members = config.map(|c| c.members).unwrap_or_default();
        let name_of = |account: Uuid| {
            members
                .iter()
                .find(|m| m.actor.account_id == Some(account))
                .map(|m| crate::display::shown(&m.actor.display_name))
                .unwrap_or_else(|| format!("account {}", &account.to_string()[..8]))
        };
        // Who the computer chose is read from its own signatures, never from
        // a certificate (anyone can certify a public device key).
        let chosen = self.device_chosen_accounts(workspace_id, request.device_id, &request.device_public_key)?;
        let someone_elses = device
            .filter(|d| !d.revoked)
            .and_then(|_| chosen.iter().find(|a| Some(**a) != me).copied())
            .map(name_of);
        let also_certified_by = device
            .filter(|d| !d.revoked)
            .map(|d| d.accounts.iter().filter(|a| Some(**a) != me && !chosen.contains(a)).map(|a| name_of(*a)).collect())
            .unwrap_or_default();
        // The proof shows only for a current human member other than you,
        // under the account key this workspace registers for them.
        let previous_member = request.previous.as_ref().and_then(|p| {
            let key = head.account_keys.get(&p.account_id)?;
            if Some(p.account_id) == me || !request.previous_verifies(key) {
                return None;
            }
            members.iter().find(|m| m.actor.kind == ActorKind::Human && m.actor.account_id == Some(p.account_id)).cloned()
        });
        Ok(LinkPreview {
            device_id: request.device_id,
            fingerprint: request.fingerprint(),
            label: crate::display::shown(&request.label),
            already_mine,
            revoked,
            someone_elses,
            also_certified_by,
            previous_member,
        })
    }

    /// Everything approving `request` checks first; the account it adds to.
    fn computer_link_checks(&self, workspace_id: Uuid, request: &LinkRequest, conn: &str, now_ms: i64) -> Result<Uuid> {
        request.check_fresh(now_ms).map_err(|e| refused(e.to_string()))?;
        if self.delegated {
            return Err(refused("only a device holding your account key can add a computer (do it on your desktop)"));
        }
        if self.author.kind != ActorKind::Human {
            return Err(refused("a box or an agent has no devices of its own to add"));
        }
        let Some(account_id) = self.author.account_id else {
            return Err(refused("this device has no account identity"));
        };
        if self.local_workspace_id == Some(workspace_id) {
            return Err(refused("this workspace lives on this device only: there is nothing to link a computer to"));
        }
        if request.device_id == self.device_id {
            return Err(refused("that is this device"));
        }
        let conn_parsed = crate::workspace_invite::decode_workspace_invite(conn).map_err(refused)?;
        if conn_parsed.id() != workspace_id || conn_parsed.inv.is_some() || conn_parsed.founder_pin().is_none() {
            return Err(refused("the connection doesn't match the workspace"));
        }
        self.require_verified_founder(workspace_id)?;
        let (config, timeline) = self.pinned_config(workspace_id)?;
        let Some(config) = config else {
            return Err(refused("the workspace configuration hasn't synced to this device yet"));
        };
        if !config.members.iter().any(|m| m.actor.id == self.author.id && m.actor.kind == ActorKind::Human) {
            return Err(refused("you aren't a member of this workspace"));
        }
        if config.guards.dearth_on().is_some() {
            return Err(refused("the workspace is in lockdown: no device can be added until an Admin turns it off"));
        }
        let head = timeline.current();
        if head.account_keys.get(&account_id).map(Vec::as_slice) != Some(&self.account_keypair.public_key_bytes()[..]) {
            return Err(refused("this desktop's account key isn't the one the workspace trusts for you"));
        }
        if head.devices.get(&self.device_id).is_none_or(|d| d.revoked || !d.accounts.contains(&account_id) || d.delegated_for(account_id)) {
            return Err(refused("this desktop isn't confirmed for the workspace yet (Settings > Passkeys & keys > Confirm this device)"));
        }
        if let Some(d) = head.devices.get(&request.device_id) {
            if d.revoked {
                return Err(refused("this computer's device was revoked here: on it, make a new request with a new device key"));
            }
            if d.accounts.contains(&account_id) {
                return Err(refused("this computer is already one of your devices here"));
            }
            // Refused only when the computer itself acted as someone else
            // here (a signature by its own key). Another member's
            // certificate of its public key alone is foreign: it gives them
            // nothing and must not block the person's own link.
            let chosen = self.device_chosen_accounts(workspace_id, request.device_id, &request.device_public_key)?;
            if chosen.iter().any(|a| *a != account_id) {
                return Err(refused(
                    "this computer is already in the workspace as someone else: on it, make a new request with a new device key",
                ));
            }
        }
        if head.passkeys.get(&account_id).is_none_or(|m| m.is_empty()) {
            return Err(refused("add a passkey first (Settings > Passkeys & keys): adding a computer is confirmed with it"));
        }
        Ok(account_id)
    }

    /// Approve `request` as one of **your** devices in `workspace_id`:
    /// publish its certificate under your account key on the config log,
    /// confirmed with your passkey (one ceremony: every account here holds a
    /// credential first), and return the grant (`ligusgrant1:…`) to paste on
    /// the computer, valid for [`GRANT_TTL_MS`] from `now_ms`. `conn` is the
    /// workspace connection (`hivews1:` codec) without an invite. The computer
    /// is a device that acts for you without your account key (delegated).
    /// Sync to publish the certificate before the computer redeems.
    pub fn issue_computer_link(&mut self, workspace_id: Uuid, request: &LinkRequest, conn: &str, now_ms: i64) -> Result<String> {
        self.computer_link_checks(workspace_id, request, conn, now_ms)?;
        self.publish_link_certificate(workspace_id, request)?;
        self.seal_link_grant(workspace_id, request, conn, now_ms)
    }

    /// Code-match approval, step one for a computer (identity model v2,
    /// 8.1): every check [`Self::issue_computer_link`] makes, then the grant,
    /// sealed now and **nothing written**. The caller offers its digest and,
    /// once the computer acknowledged, calls [`Self::publish_computer_link`].
    pub fn prepare_computer_link(&self, workspace_id: Uuid, request: &LinkRequest, conn: &str, now_ms: i64) -> Result<String> {
        self.computer_link_checks(workspace_id, request, conn, now_ms)?;
        self.seal_link_grant(workspace_id, request, conn, now_ms)
    }

    /// Code-match approval, step two: the checks again, then the
    /// certificate on the config log with the passkey ceremony, drafted
    /// fresh now. Sync to publish it.
    pub fn publish_computer_link(&mut self, workspace_id: Uuid, request: &LinkRequest, conn: &str, now_ms: i64) -> Result<()> {
        self.computer_link_checks(workspace_id, request, conn, now_ms)?;
        self.publish_link_certificate(workspace_id, request)
    }

    fn publish_link_certificate(&mut self, workspace_id: Uuid, request: &LinkRequest) -> Result<()> {
        let account_id = self.author.account_id.ok_or_else(|| refused("this device has no account identity"))?;
        let config_id = workspace_config_session_id(workspace_id);
        let certificate = DeviceCertificate::issue(&self.account_keypair, account_id, &request.device_public_key, Timestamp::now());
        // The account holds a credential, so the rules ask its presence for a
        // new device: `append_signed` runs the ceremony (the desktop's
        // passkey, the CLI's recovery key) or refuses.
        self.append_signed(config_id, workspace_id, SessionEvent::DeviceCertificateAdded { certificate })?;
        Ok(())
    }

    fn seal_link_grant(&self, workspace_id: Uuid, request: &LinkRequest, conn: &str, now_ms: i64) -> Result<String> {
        let person = crate::computer_link::device_local_cleared(&self.author);
        Grant::seal(request, workspace_id, conn, &person, self.device_id, &self.keypair, now_ms.saturating_add(GRANT_TTL_MS))
            .map_err(|e| refused(e.to_string()))
    }

    /// On the new computer, after the grant's workspace has been pulled (with
    /// the grant's founder pin): whether the workspace agrees with the grant.
    /// The founding is verified; the person is a human member; the approving
    /// device is theirs (certified, not delegated, not revoked, its key the
    /// grant's); this device is certified for the person, never acted for
    /// anyone else here, and isn't revoked; no lockdown. A certificate
    /// another member put on this device's key is ignored (it gives them
    /// nothing). Writes nothing. Returns the person (to
    /// `act_as_owner_device`).
    pub fn check_computer_link(&self, grant: &OpenedGrant, now_ms: i64) -> Result<ActorIdentity> {
        let person = grant.check(&self.keypair.public_key_bytes(), now_ms).map_err(|e| refused(e.to_string()))?;
        let ws = grant.workspace_id;
        let account = person.account_id.ok_or_else(|| refused("the grant names no account"))?;
        if self.waiting_for_founder(ws)?.is_some() {
            return Err(refused("the workspace's founding hasn't synced or doesn't match its founder: sync and try again"));
        }
        let (config, timeline) = self.pinned_config(ws)?;
        let Some(config) = config else {
            return Err(refused("the workspace hasn't synced to this computer yet: check the relay and try again"));
        };
        let head = timeline.current();
        if !config.members.iter().any(|m| m.actor.kind == ActorKind::Human && m.actor.account_id == Some(account) && m.actor.id == person.id) {
            return Err(refused("the person who approved isn't a member of this workspace"));
        }
        let approver_ok = head.devices.get(&grant.approver_device_id).is_some_and(|d| {
            !d.revoked && !d.phone && d.accounts.contains(&account) && !d.delegated_for(account) && d.public_key == grant.approver_key
        });
        if !approver_ok {
            return Err(refused("the grant wasn't issued by a device of that person in this workspace"));
        }
        if config.guards.dearth_on().is_some() {
            return Err(refused("the workspace is in lockdown: no device can be added until an Admin turns it off"));
        }
        let Some(me) = head.devices.get(&self.device_id) else {
            return Err(refused(crate::computer_link::NOT_ON_RELAY_YET));
        };
        if me.revoked {
            return Err(refused("this computer's device was revoked in this workspace: make a new request with a new device key"));
        }
        let chosen = self.device_chosen_accounts(ws, self.device_id, &self.keypair.public_key_bytes())?;
        if chosen.iter().any(|a| *a != account) {
            return Err(refused("this computer is already in the workspace as someone else: make a new request with a new device key"));
        }
        if !me.accounts.contains(&account) {
            return Err(refused(crate::computer_link::NOT_ON_RELAY_YET));
        }
        Ok(person)
    }
}
