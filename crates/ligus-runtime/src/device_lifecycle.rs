//! A device's state as My devices and People show it (workspace model v3,
//! 10.1): **Pending** (approved, nothing written yet), **Active**, **Idle**
//! (no signature and no beat for [`IDLE_AFTER_DAYS`] days, a view) and
//! **Revoked**. Derived from events already on the log and from presence;
//! nothing new is written. Pending and Idle devices are offered Remove (the
//! existing `DeviceRevoked`, with a passkey).

use ligus_core::Timestamp;

/// Days without a signature or a beat after which a device shows as Idle.
pub const IDLE_AFTER_DAYS: i64 = 30;

/// One device's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Pending,
    Active,
    Idle,
    Revoked,
}

impl DeviceState {
    /// The wire name the desktop and lt read.
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceState::Pending => "pending",
            DeviceState::Active => "active",
            DeviceState::Idle => "idle",
            DeviceState::Revoked => "revoked",
        }
    }
}

/// What a device's state is read from.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeviceFacts {
    pub revoked: bool,
    /// Approved here but it never wrote anything
    /// ([`crate::ChatService::never_written`]).
    pub never_written: bool,
    /// This device itself: always Active.
    pub this_device: bool,
    /// Its newest signed event here.
    pub last_signed: Option<Timestamp>,
    /// Its newest beat (a host's presence, a phone's pairing seen by a box).
    pub last_beat: Option<Timestamp>,
    /// When it was certified for this account (a fresh device isn't Idle).
    pub certified_at: Option<Timestamp>,
}

/// The state of a device with `facts`, as of `now`.
pub fn state_of(facts: &DeviceFacts, now: Timestamp) -> DeviceState {
    if facts.revoked {
        return DeviceState::Revoked;
    }
    if facts.this_device {
        return DeviceState::Active;
    }
    if facts.never_written {
        return DeviceState::Pending;
    }
    let newest = [facts.last_signed, facts.last_beat, facts.certified_at].into_iter().flatten().max();
    match newest {
        Some(at) if (now.inner() - at.inner()).whole_days() < IDLE_AFTER_DAYS => DeviceState::Active,
        _ => DeviceState::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days_ago(now: Timestamp, d: i64) -> Option<Timestamp> {
        Some(Timestamp::from_unix_seconds(now.inner().unix_timestamp() - d * 86_400))
    }

    #[test]
    fn states_follow_the_note() {
        let now = Timestamp::from_unix_seconds(1_800_000_000);
        let f = |d: DeviceFacts| state_of(&d, now);
        assert_eq!(f(DeviceFacts { revoked: true, this_device: true, ..Default::default() }), DeviceState::Revoked);
        assert_eq!(f(DeviceFacts { this_device: true, ..Default::default() }), DeviceState::Active);
        assert_eq!(f(DeviceFacts { never_written: true, last_beat: days_ago(now, 1), ..Default::default() }), DeviceState::Pending);
        assert_eq!(f(DeviceFacts { last_signed: days_ago(now, 3), ..Default::default() }), DeviceState::Active);
        assert_eq!(f(DeviceFacts { last_signed: days_ago(now, 45), last_beat: days_ago(now, 2), ..Default::default() }), DeviceState::Active, "a beat keeps it active");
        assert_eq!(f(DeviceFacts { last_signed: days_ago(now, 31), last_beat: days_ago(now, 40), ..Default::default() }), DeviceState::Idle);
        assert_eq!(f(DeviceFacts { certified_at: days_ago(now, 5), ..Default::default() }), DeviceState::Active, "just added");
        assert_eq!(f(DeviceFacts::default()), DeviceState::Idle, "nothing known");
        assert_eq!(f(DeviceFacts { last_signed: days_ago(now, IDLE_AFTER_DAYS), ..Default::default() }), DeviceState::Idle);
        assert_eq!(f(DeviceFacts { last_signed: days_ago(now, IDLE_AFTER_DAYS - 1), ..Default::default() }), DeviceState::Active);
    }
}
