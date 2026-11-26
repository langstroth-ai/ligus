//! "Which person is this device" per workspace (identity model v2, 7.2),
//! from the log, with `computer-link.json` only as a cache.
//!
//! A device acts for the account it chose itself: its own verified
//! signature as that account, which the account certified
//! ([`crate::ChatService::device_chosen_accounts`]), or its own redeem (the
//! cache entry the redeem wrote). A certificate someone else publishes adds
//! nothing, and no decision here ever reads a display name.

use uuid::Uuid;

/// Where this device stands in one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// Acts as this install's own account (never linked anywhere).
    Own,
    /// Acts as the person, as one of their delegated devices.
    Linked { account: Uuid },
    /// Linked as a person's device, but not in this workspace: read-only,
    /// with "Link here" (decision O7).
    NotLinkedHere,
    /// Linked here per the cache, but the person's certificate hasn't reached
    /// this device yet: read-only.
    Pending { account: Uuid },
    /// Certified, but the person is no longer a member here: read-only.
    PersonRemoved { account: Uuid },
    /// Revoked here.
    Revoked,
    /// This device signed as two people here (only after the cache was
    /// lost): a person must pick.
    Ambiguous(Vec<Uuid>),
}

impl Binding {
    /// Whether writes are allowed.
    pub fn writable(&self) -> bool {
        matches!(self, Binding::Own | Binding::Linked { .. })
    }

    /// The account to act as, when linked.
    pub fn account(&self) -> Option<Uuid> {
        match self {
            Binding::Linked { account } => Some(*account),
            _ => None,
        }
    }

    /// Why writes are blocked, in words a person reads.
    pub fn why_read_only(&self) -> Option<String> {
        match self {
            Binding::Own | Binding::Linked { .. } => None,
            Binding::NotLinkedHere => Some(NOT_LINKED_HERE.into()),
            Binding::Pending { .. } => Some(crate::computer_link::NOT_ON_RELAY_YET.into()),
            Binding::PersonRemoved { .. } => Some("the person this computer acts for isn't a member of this workspace any more".into()),
            Binding::Revoked => Some("this computer's device was revoked in this workspace".into()),
            Binding::Ambiguous(_) => Some("this computer acted as two people here: choose one with `ligus devices use ACCOUNT`".into()),
        }
    }
}

/// Why a linked computer is read-only in a workspace it wasn't linked in.
pub const NOT_LINKED_HERE: &str = "this computer is one of your devices, but isn't linked into this workspace: link it here from your desktop (My devices > Add a computer)";

/// The pure decision of 7.2, given what the log says.
///
/// - `revoked`: this device is revoked here;
/// - `certified`: accounts whose certificate of this device counts here,
///   this install's own account left out;
/// - `chosen`: accounts this device signed as here (verified, certified);
/// - `members`: accounts that are human members here;
/// - `cache`: the account `computer-link.json` names for this workspace;
/// - `person`: the install's one person, if linked anywhere;
/// - `linked_anywhere`: whether the install is linked anywhere.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    revoked: bool,
    certified: &std::collections::BTreeSet<Uuid>,
    chosen: &std::collections::BTreeSet<Uuid>,
    members: &std::collections::BTreeSet<Uuid>,
    cache: Option<Uuid>,
    person: Option<Uuid>,
    linked_anywhere: bool,
) -> Binding {
    if revoked {
        return Binding::Revoked;
    }
    let linked = |a: Uuid| {
        if members.contains(&a) {
            Binding::Linked { account: a }
        } else {
            Binding::PersonRemoved { account: a }
        }
    };
    if let Some(a) = cache {
        return if certified.contains(&a) {
            linked(a)
        } else {
            Binding::Pending { account: a }
        };
    }
    let both: Vec<Uuid> = chosen
        .iter()
        .filter(|a| certified.contains(a))
        .copied()
        .collect();
    match person {
        // Only the install's one person is ever adopted.
        Some(p) if both.contains(&p) => linked(p),
        Some(_) => Binding::NotLinkedHere,
        // The whole file was lost: the device's own choice, if single.
        None if !linked_anywhere => match both.as_slice() {
            [] => Binding::Own,
            [a] => linked(*a),
            many => Binding::Ambiguous(many.to_vec()),
        },
        None => Binding::NotLinkedHere,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn set(v: &[Uuid]) -> BTreeSet<Uuid> {
        v.iter().copied().collect()
    }

    /// B4: the cache heals; a cache naming an uncertified account gives
    /// Pending; two chosen accounts give Ambiguous; planted (unchosen)
    /// certificates never adopt; a foreign certifier is ignored.
    #[test]
    fn the_binding_follows_the_devices_own_choice() {
        let (ana, ben) = (Uuid::new_v4(), Uuid::new_v4());
        let members = set(&[ana, ben]);
        assert_eq!(
            decide(
                false,
                &set(&[ana]),
                &set(&[ana]),
                &members,
                Some(ana),
                Some(ana),
                true
            ),
            Binding::Linked { account: ana }
        );
        assert_eq!(
            decide(
                false,
                &set(&[]),
                &set(&[]),
                &members,
                Some(ana),
                Some(ana),
                true
            ),
            Binding::Pending { account: ana }
        );
        // Lost cache entry: heals from the device's own signatures.
        assert_eq!(
            decide(
                false,
                &set(&[ana]),
                &set(&[ana]),
                &members,
                None,
                Some(ana),
                true
            ),
            Binding::Linked { account: ana }
        );
        // A foreign certificate (Ben) the device never chose: not adopted.
        assert_eq!(
            decide(
                false,
                &set(&[ben]),
                &set(&[]),
                &members,
                None,
                Some(ana),
                true
            ),
            Binding::NotLinkedHere
        );
        assert_eq!(
            decide(false, &set(&[ben]), &set(&[]), &members, None, None, false),
            Binding::Own
        );
        // Even chosen, Ben isn't the install's person.
        assert_eq!(
            decide(
                false,
                &set(&[ana, ben]),
                &set(&[ben]),
                &members,
                None,
                Some(ana),
                true
            ),
            Binding::NotLinkedHere
        );
        // The whole file lost, two chosen: a person picks.
        assert!(matches!(
            decide(
                false,
                &set(&[ana, ben]),
                &set(&[ana, ben]),
                &members,
                None,
                None,
                false
            ),
            Binding::Ambiguous(_)
        ));
        // Removed person; revoked device.
        assert_eq!(
            decide(
                false,
                &set(&[ana]),
                &set(&[ana]),
                &set(&[ben]),
                Some(ana),
                Some(ana),
                true
            ),
            Binding::PersonRemoved { account: ana }
        );
        assert_eq!(
            decide(
                true,
                &set(&[ana]),
                &set(&[ana]),
                &members,
                Some(ana),
                Some(ana),
                true
            ),
            Binding::Revoked
        );
    }
}
