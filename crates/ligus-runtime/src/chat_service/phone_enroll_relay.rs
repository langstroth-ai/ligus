//! Reverse-QR phone enrollment on the roster
//! (`docs/design/v2/phone-enroll-reverse-qr.md`): who may sign the presence
//! slots (§5.5, review R2), what a box checks on its roster before it relays
//! a request (§7.2, §7.3), and the slot signatures with this device's key.
//! The wire formats and the pure checks are [`crate::phone_enroll_qr`]; this
//! is the part that needs the folded config log.
//!
//! - A **request** slot counts only when signed by a certified, unrevoked,
//!   non-phone device of a current **service member** (a box):
//!   [`ChatService::enroll_relay_key`]. A person's own device can't inject a
//!   request, even holding the workspace key and the QR's secret.
//! - An **answer** slot counts only when signed by the offer's own desktop
//!   device: [`ChatService::enroll_desktop_key`], a certified, unrevoked,
//!   non-phone, non-delegated device of exactly one current human member.

use ligus_core::{ActorKind, WorkspaceRole};
use uuid::Uuid;

use super::ChatService;
use crate::phone_enroll_qr::{BoxRefusal, RelayedAnswer, RelayedRequest};

/// The person whose desktop showed an offer, as this device's roster has
/// them (`POST /v1/phone-enroll/check`'s `person` and `desktopLabel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollDesktop {
    pub account_id: Uuid,
    pub member_id: String,
    pub display_name: String,
    pub role: WorkspaceRole,
    /// Another current member has the same display name (after
    /// `clean_display_name`, ignoring case): the phone warns (review R4).
    pub name_collision: bool,
    /// The desktop's host label, when it registered one (empty otherwise).
    pub label: String,
}

/// Where the phone in a request stands for the desktop's person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollPhoneStanding {
    /// Not on the roster: the desktop can certify it.
    New,
    /// Already certified for this person with this key: nothing to do.
    AlreadyMine,
}

impl EnrollPhoneStanding {
    /// The `phone.standing` field: `new` or `alreadyMine`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::AlreadyMine => "alreadyMine",
        }
    }
}

impl ChatService {
    /// The key of `device` when it may sign a `phone-enroll:*` request slot:
    /// certified, unrevoked, not a phone, held by a current **service**
    /// member (a box). `None` for anyone else (review R2).
    pub fn enroll_relay_key(&self, workspace_id: Uuid, device: Uuid) -> Option<Vec<u8>> {
        let (_, t) = self
            .store
            .load_config(
                workspace_id,
                self.trusted_founder.as_deref(),
                self.local_workspace_id,
            )
            .ok()?;
        let roster = t.current();
        let d = roster
            .devices
            .get(&device)
            .filter(|d| !d.revoked && !d.phone)?;
        let service = roster
            .members
            .values()
            .filter(|m| m.kind == ActorKind::Service)
            .filter_map(|m| m.account_id)
            .any(|a| d.accounts.contains(&a) && d.holders.contains(&a));
        if !service {
            return None;
        }
        self.certified_device_key(workspace_id, device)
            .ok()
            .flatten()
    }

    /// The person whose device `desktop` is: a certified, unrevoked,
    /// non-phone device held, not as a delegate, by exactly one current
    /// human member. Otherwise [`BoxRefusal::DesktopUnknown`].
    pub fn enroll_desktop(
        &self,
        workspace_id: Uuid,
        desktop: Uuid,
    ) -> Result<EnrollDesktop, BoxRefusal> {
        let (config, t) = self
            .store
            .load_config(
                workspace_id,
                self.trusted_founder.as_deref(),
                self.local_workspace_id,
            )
            .map_err(|_| BoxRefusal::DesktopUnknown)?;
        let roster = t.current();
        let d = roster
            .devices
            .get(&desktop)
            .filter(|d| !d.revoked && !d.phone)
            .ok_or(BoxRefusal::DesktopUnknown)?;
        let mut people = roster.members.iter().filter(|(_, m)| {
            m.kind == ActorKind::Human
                && m.account_id.is_some_and(|a| {
                    d.accounts.contains(&a) && d.holders.contains(&a) && !d.delegated_for(a)
                })
        });
        let (member_id, member) = match (people.next(), people.next()) {
            (Some(p), None) => p,
            _ => return Err(BoxRefusal::DesktopUnknown),
        };
        let account_id = member.account_id.ok_or(BoxRefusal::DesktopUnknown)?;
        if self
            .certified_device_key(workspace_id, desktop)
            .ok()
            .flatten()
            .is_none()
        {
            return Err(BoxRefusal::DesktopUnknown);
        }
        let name_of = |id: &str| {
            config
                .as_ref()
                .and_then(|c| c.members.iter().find(|m| m.id == id))
                .map(|m| crate::display::shown(&m.actor.display_name))
                .unwrap_or_default()
        };
        let display_name = name_of(member_id);
        let folded = display_name.to_lowercase();
        let name_collision = !folded.is_empty()
            && roster
                .members
                .keys()
                .filter(|id| *id != member_id)
                .any(|id| name_of(id).to_lowercase() == folded);
        let label = self
            .list_hosts(workspace_id)
            .ok()
            .and_then(|hs| hs.into_iter().find(|h| h.id == desktop.to_string()))
            .map(|h| crate::display::shown(&h.label))
            .unwrap_or_default();
        Ok(EnrollDesktop {
            account_id,
            member_id: member_id.clone(),
            display_name,
            role: member.role,
            name_collision,
            label,
        })
    }

    /// The key of `desktop` when it may sign a `phone-enroll-answer:*` slot:
    /// the device [`Self::enroll_desktop`] accepts, with its certified key.
    pub fn enroll_desktop_key(&self, workspace_id: Uuid, desktop: Uuid) -> Option<Vec<u8>> {
        self.enroll_desktop(workspace_id, desktop).ok()?;
        self.certified_device_key(workspace_id, desktop)
            .ok()
            .flatten()
    }

    /// Where the phone (`device`, and `public_key` when known: the code in
    /// a request) stands for `account` (the desktop's person): refused when
    /// it is revoked here ([`BoxRefusal::DeviceRevoked`]) or is someone
    /// else's, certified with another key, or not a phone
    /// ([`BoxRefusal::SomeoneElses`]).
    pub fn enroll_phone_standing(
        &self,
        workspace_id: Uuid,
        device: Uuid,
        public_key: Option<&[u8]>,
        account: Uuid,
    ) -> Result<EnrollPhoneStanding, BoxRefusal> {
        let (_, t) = self
            .store
            .load_config(
                workspace_id,
                self.trusted_founder.as_deref(),
                self.local_workspace_id,
            )
            .map_err(|_| BoxRefusal::DesktopUnknown)?;
        match t.current().devices.get(&device) {
            None => Ok(EnrollPhoneStanding::New),
            Some(d) if d.revoked => Err(BoxRefusal::DeviceRevoked),
            Some(d) if !d.phone || public_key.is_some_and(|k| d.public_key != k) => {
                Err(BoxRefusal::SomeoneElses)
            }
            Some(d) if d.accounts.contains(&account) => Ok(EnrollPhoneStanding::AlreadyMine),
            Some(_) => Err(BoxRefusal::SomeoneElses),
        }
    }

    /// Sign a request slot with this device's key (a box relaying a phone's request).
    pub fn sign_relayed_request(&self, r: RelayedRequest) -> RelayedRequest {
        r.sign(&self.keypair)
    }

    /// Sign an answer slot with this device's key (the desktop that showed the offer).
    pub fn sign_relayed_answer(&self, a: RelayedAnswer) -> RelayedAnswer {
        a.sign(&self.keypair)
    }
}
