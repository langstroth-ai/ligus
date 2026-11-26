//! Who may write a profile or a device label (identity model v2, 5.2 and
//! 6.2). Both are render-only: nothing else in the fold reads them.

use crate::events::SessionEventEnvelope;
use crate::identity::ActorKind;
use crate::profile::{self, DeviceLabel, ProfileChange};

use super::RosterState;

/// `ProfileUpdated`: on the config log, by a person or a service member for
/// their own account, signed by a device certified for that account here
/// that is neither delegated (a linked computer, a box acting for them) nor
/// a phone, so a stolen linked computer or phone can't rename its person
/// everywhere; never an agent or a runtime effect. Then the content checks.
pub fn profile_ok(
    env: &SessionEventEnvelope,
    change: &ProfileChange,
    on_config: bool,
    roster: &RosterState,
) -> Result<(), String> {
    if !on_config {
        return Err("a profile is set on the workspace config log".into());
    }
    let stamp = env
        .actor_stamp
        .as_ref()
        .ok_or("a profile names its author")?;
    if !matches!(stamp.actor.kind, ActorKind::Human | ActorKind::Service) || stamp.runtime_effect {
        return Err("only a person or a box sets a profile".into());
    }
    if stamp.actor.account_id != Some(change.account_id) {
        return Err("a profile is set by its own account".into());
    }
    let device = super::certified_signer(env, roster)?;
    if device.phone {
        return Err("a phone can't change its person's profile".into());
    }
    if device.delegated_for(change.account_id) {
        return Err(
            "a linked device can't change its person's profile: change it on their desktop".into(),
        );
    }
    profile::change_ok(change).map_err(String::from)
}

/// `DeviceLabelSet`: on the config log, for the stamp's own account, of a
/// device certified for that account here and not revoked, signed either by
/// that device itself or by a non-delegated, non-phone device of the
/// account (the person's desktop renaming their phone or computer). Labels
/// are keyed by (device, account), so a member who certified someone else's
/// device key for themselves only ever labels it under their own name.
pub fn label_ok(
    env: &SessionEventEnvelope,
    label: &DeviceLabel,
    on_config: bool,
    roster: &RosterState,
) -> Result<(), String> {
    if !on_config {
        return Err("a device name is set on the workspace config log".into());
    }
    let stamp = env
        .actor_stamp
        .as_ref()
        .ok_or("a device name names its author")?;
    if !matches!(stamp.actor.kind, ActorKind::Human | ActorKind::Service) || stamp.runtime_effect {
        return Err("only a person or a box names a device".into());
    }
    if stamp.actor.account_id != Some(label.account_id) {
        return Err("a device is named under its author's own account".into());
    }
    let named = roster
        .devices
        .get(&label.device_id)
        .filter(|d| !d.revoked && d.accounts.contains(&label.account_id))
        .ok_or("that device isn't certified for this account here")?;
    let signer = super::certified_signer(env, roster)?;
    let itself = env.signer_device_id == Some(label.device_id) && !named.phone;
    if !itself && (signer.delegated_for(label.account_id) || signer.phone) {
        return Err("a device is named by itself or from one of its person's desktops".into());
    }
    profile::label_ok(&label.label).map_err(String::from)
}
