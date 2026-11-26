//! Writing profiles and device labels (identity model v2, 5.5 and 6.3).

use ligus_core::profile::{Avatar, DeviceLabel, ProfileChange};
use ligus_core::{workspace_config_session_id, SessionEvent};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// At most one profile change a minute and twenty a day, per account and
/// workspace (5.6): a client throttle on top of the fold's content checks.
pub const PROFILE_MIN_INTERVAL_MS: i64 = 60_000;
pub const PROFILE_MAX_PER_DAY: usize = 20;

fn refused(why: impl Into<String>) -> ChatError {
    ChatError::Unauthorized(why.into())
}

impl ChatService {
    /// Change this account's name and/or picture in `workspace_id`. Only a
    /// device holding the account key (never a linked computer or a box
    /// acting for someone) may. `None` leaves a field as it is. Returns
    /// whether anything was written (nothing changed: nothing written).
    pub fn set_profile(
        &mut self,
        workspace_id: Uuid,
        display_name: Option<&str>,
        avatar: Option<Avatar>,
        clear_avatar: bool,
    ) -> Result<bool> {
        if self.delegated {
            return Err(refused(
                "change your name and picture on your desktop: a linked device can't",
            ));
        }
        let account = self
            .author
            .account_id
            .ok_or_else(|| refused("this device has no account"))?;
        let config = self.load_in(workspace_id, workspace_config_session_id(workspace_id))?;
        let Some(config) = config else {
            return Ok(false);
        };
        if !config
            .members
            .iter()
            .any(|m| m.actor.account_id == Some(account))
        {
            return Ok(false);
        }
        let current = config.profiles.get(&account).cloned().unwrap_or_default();
        let name = display_name
            .map(ligus_proto::names::clean_display_name)
            .filter(|n| !n.is_empty());
        let name = name.filter(|n| current.display_name.as_deref() != Some(n.as_str()));
        let avatar = avatar.filter(|a| current.avatar.as_ref() != Some(a));
        let clear_avatar = clear_avatar && avatar.is_none() && current.avatar.is_some();
        if name.is_none() && avatar.is_none() && !clear_avatar {
            return Ok(false);
        }
        let change = ProfileChange {
            account_id: account,
            display_name: name,
            avatar,
            clear_avatar,
            issued_at_ms: ligus_core::flow::now_ms(),
        };
        ligus_core::profile::change_ok(&change).map_err(refused)?;
        self.profile_throttle(workspace_id, account)?;
        self.append_signed(
            workspace_config_session_id(workspace_id),
            workspace_id,
            SessionEvent::ProfileUpdated { profile: change },
        )?;
        Ok(true)
    }

    /// Refuse a profile change past the client throttle (5.6).
    fn profile_throttle(&self, workspace_id: Uuid, account: Uuid) -> Result<()> {
        let now = ligus_core::flow::now_ms();
        let mine: Vec<i64> = self
            .store
            .config_envelopes(workspace_id)?
            .into_iter()
            .filter_map(|e| match &e.payload {
                SessionEvent::ProfileUpdated { profile } if profile.account_id == account => {
                    Some(profile.issued_at_ms)
                }
                _ => None,
            })
            .collect();
        if mine
            .iter()
            .any(|t| now - t < PROFILE_MIN_INTERVAL_MS && *t <= now)
        {
            return Err(refused(
                "you changed your profile a moment ago: try again in a minute",
            ));
        }
        if mine
            .iter()
            .filter(|t| now - **t < 86_400_000 && **t <= now)
            .count()
            >= PROFILE_MAX_PER_DAY
        {
            return Err(refused(
                "that's 20 profile changes today: try again tomorrow",
            ));
        }
        Ok(())
    }

    /// [`Self::set_profile`] in every workspace this device holds where this
    /// account is a member. Returns the workspaces written, and the first
    /// error, if any (the others are still tried).
    pub fn set_profile_everywhere(
        &mut self,
        display_name: Option<&str>,
        avatar: Option<Avatar>,
        clear_avatar: bool,
    ) -> (Vec<Uuid>, Option<ChatError>) {
        let mut done = Vec::new();
        let mut first = None;
        let workspaces = self.store.pinned_workspaces().unwrap_or_default();
        for ws in workspaces {
            match self.set_profile(ws, display_name, avatar.clone(), clear_avatar) {
                Ok(true) => done.push(ws),
                Ok(false) => {}
                Err(e) => {
                    if first.is_none() {
                        first = Some(e);
                    }
                }
            }
        }
        (done, first)
    }

    /// Name `device` (one of this account's, or this device itself) in
    /// `workspace_id`; an empty `label` clears it. Returns whether anything
    /// was written.
    pub fn set_device_label(
        &mut self,
        workspace_id: Uuid,
        device: Uuid,
        label: &str,
    ) -> Result<bool> {
        let account = self
            .author
            .account_id
            .ok_or_else(|| refused("this device has no account"))?;
        if device != self.device_id && self.delegated {
            return Err(refused("rename your other devices on your desktop"));
        }
        let label = ligus_proto::names::clean_display_name(label);
        ligus_core::profile::label_ok(&label).map_err(refused)?;
        let config = self.load_in(workspace_id, workspace_config_session_id(workspace_id))?;
        let current = config
            .as_ref()
            .and_then(|c| c.device_labels.get(&device))
            .and_then(|m| m.get(&account))
            .cloned()
            .unwrap_or_default();
        if current == label {
            return Ok(false);
        }
        self.append_signed(
            workspace_config_session_id(workspace_id),
            workspace_id,
            SessionEvent::DeviceLabelSet {
                label: DeviceLabel {
                    device_id: device,
                    account_id: account,
                    label,
                },
            },
        )?;
        Ok(true)
    }

    /// The synced name of `device` under `account` in `workspace_id`, if one
    /// was set (6.3).
    pub fn device_label_of(
        &self,
        workspace_id: Uuid,
        device: Uuid,
        account: Uuid,
    ) -> Option<String> {
        self.load_in(workspace_id, workspace_config_session_id(workspace_id))
            .ok()
            .flatten()?
            .device_labels
            .get(&device)?
            .get(&account)
            .cloned()
    }
}

impl ChatService {
    /// Which person this device is in `workspace_id` (identity model v2,
    /// 7.2): see [`crate::device_binding::decide`]. `local_account` is this
    /// install's own account; `link` its `computer-link.json`.
    pub fn binding(
        &self,
        workspace_id: Uuid,
        link: &crate::computer_link::LinkState,
        local_account: Uuid,
    ) -> Result<crate::device_binding::Binding> {
        let (config, timeline) = self.store.load_config(
            workspace_id,
            self.trusted_founder.as_deref(),
            self.local_workspace_id,
        )?;
        let head = timeline.current();
        let revoked = config
            .as_ref()
            .is_some_and(|c| c.guards.revoked_devices.contains(&self.device_id))
            || head.devices.get(&self.device_id).is_some_and(|d| d.revoked);
        let certified: std::collections::BTreeSet<Uuid> = head
            .devices
            .get(&self.device_id)
            .map(|d| {
                d.accounts
                    .iter()
                    .copied()
                    .filter(|a| *a != local_account)
                    .collect()
            })
            .unwrap_or_default();
        let chosen = self.device_chosen_accounts(
            workspace_id,
            self.device_id,
            &self.keypair.public_key_bytes(),
        )?;
        let members: std::collections::BTreeSet<Uuid> = config
            .as_ref()
            .map(|c| {
                c.members
                    .iter()
                    .filter(|m| m.actor.kind == ligus_core::ActorKind::Human)
                    .filter_map(|m| m.actor.account_id)
                    .collect()
            })
            .unwrap_or_default();
        let person = link.person.as_ref().and_then(|p| p.account_id);
        Ok(crate::device_binding::decide(
            revoked,
            &certified,
            &chosen,
            &members,
            link.account_in(workspace_id),
            person,
            link.is_linked(),
        ))
    }

    /// The actor to act as for `account` in `workspace_id`: its member
    /// record there with its current profile name (never a frozen copy),
    /// else `fallback`.
    pub fn person_actor(
        &self,
        workspace_id: Uuid,
        account: Uuid,
        fallback: Option<&ligus_core::ActorIdentity>,
    ) -> Option<ligus_core::ActorIdentity> {
        let config = self
            .load_in(workspace_id, workspace_config_session_id(workspace_id))
            .ok()
            .flatten();
        let from_roster = config.as_ref().and_then(|c| {
            c.members
                .iter()
                .find(|m| m.actor.account_id == Some(account))
                .map(|m| m.actor.clone())
        });
        from_roster
            .or_else(|| fallback.filter(|f| f.account_id == Some(account)).cloned())
            .map(|a| crate::computer_link::device_local_cleared(&a))
    }
}

/// How long a link may take before an approved computer that wrote nothing
/// counts as never finished (request 15 + grant 30 minutes; 8.2).
pub const NEVER_FINISHED_AFTER_MS: i64 = 45 * 60 * 1000;

impl ChatService {
    /// When `device`, a computer this account approved here (a certificate
    /// another device of the account published for it), was approved, if
    /// it never signed anything as this account here and that was more than
    /// [`NEVER_FINISHED_AFTER_MS`] ago by this device's clock: "Approved on
    /// …, hasn't written anything yet" (identity model v2, 8.2; O4). Phones
    /// never sign until used, so they are left out. Only the device's own
    /// verified signatures count.
    pub fn never_written(
        &self,
        workspace_id: Uuid,
        device: Uuid,
    ) -> Result<Option<ligus_core::Timestamp>> {
        self.never_written_at(workspace_id, device, ligus_core::Timestamp::now())
    }

    /// [`Self::never_written`] as of `now`.
    pub fn never_written_at(
        &self,
        workspace_id: Uuid,
        device: Uuid,
        now: ligus_core::Timestamp,
    ) -> Result<Option<ligus_core::Timestamp>> {
        let Some(account) = self.author.account_id else {
            return Ok(None);
        };
        if device == self.device_id {
            return Ok(None);
        }
        let (_, timeline) = self.store.load_config(
            workspace_id,
            self.trusted_founder.as_deref(),
            self.local_workspace_id,
        )?;
        let key = timeline
            .current()
            .account_keys
            .get(&account)
            .cloned()
            .unwrap_or_default();
        let cert = self
            .store
            .trust_envelopes(workspace_id)?
            .into_iter()
            .find_map(|e| match &e.payload {
                SessionEvent::DeviceCertificateAdded { certificate: c }
                    if c.device_id == device
                        && c.account_id == account
                        && c.alg == ligus_core::crypto::SigAlg::Ed25519
                        && c.verify(&key).is_ok()
                        && e.signer_device_id.is_some_and(|s| s != device) =>
                {
                    Some(c.clone())
                }
                _ => None,
            });
        let Some(cert) = cert else { return Ok(None) };
        if !self
            .device_signed_accounts(workspace_id, device, &cert.device_public_key)?
            .is_empty()
        {
            return Ok(None);
        }
        let age_ms = (now.inner() - cert.issued_at.inner()).whole_milliseconds();
        Ok((age_ms > NEVER_FINISHED_AFTER_MS as i128).then_some(cert.issued_at))
    }
}

/// A device whose certificate would stop counting under rules 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules3Gap {
    pub device_id: Uuid,
    pub account_id: Uuid,
    pub phone: bool,
}

impl ChatService {
    /// The readiness list for switching `workspace_id` to rules 3 (decision
    /// O6): every device certified here today whose device never accepted
    /// that certificate, so it would stop counting (a computer that never
    /// redeemed, a phone two accounts certified without its statement, a
    /// certificate someone put on another device's key). Empty when the
    /// switch changes nothing for anyone.
    pub fn rules3_readiness(&self, workspace_id: Uuid) -> Result<Vec<Rules3Gap>> {
        let (_, timeline) = self.store.load_config(
            workspace_id,
            self.trusted_founder.as_deref(),
            self.local_workspace_id,
        )?;
        let envs = self.store.config_envelopes(workspace_id)?;
        let refs: Vec<&ligus_core::SessionEventEnvelope> = envs.iter().collect();
        let accepted = ligus_core::authz::RosterTracker::accepted(&refs, workspace_id);
        let head = timeline.current();
        let mut out = Vec::new();
        for (device, d) in &head.devices {
            if d.revoked {
                continue;
            }
            for account in &d.accounts {
                if !accepted.contains(&(*device, *account))
                    && head
                        .members
                        .values()
                        .any(|m| m.account_id == Some(*account))
                {
                    out.push(Rules3Gap {
                        device_id: *device,
                        account_id: *account,
                        phone: d.phone,
                    });
                }
            }
        }
        Ok(out)
    }
}
