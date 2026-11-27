//! Leaving a workspace on your own: `MemberLeft` (workspace model v3, 10.4).
//! Under rules 3 it is display only: the member is listed as "asked to
//! leave" until an Admin removes them, and keeps the keys it holds. Leaving
//! never revokes this device or any other: that is `DeviceRevoked`, or an
//! Admin's removal and a key rotation.

use ligus_core::{workspace_config_session_id, SessionEvent};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// The one line every surface shows before leaving (desktop, lt, `ligus
/// serve leave`).
pub const LEAVE_CONSEQUENCE: &str = "Others see you as asked to leave until an Admin removes you, and this device stops syncing the workspace.";
/// The reminder that goes with it.
pub const NOT_REVOKING: &str = "Leaving isn't revoking: your other devices and anything they already copied keep their keys. Revoke a lost device in My devices.";

/// What leaving would do here, judged as every fold will.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveCheck {
    /// It can be written now (without a passkey).
    Ready,
    /// It can be written with this account's passkey.
    NeedsPasskey,
    /// Already listed as asked to leave.
    AlreadyLeft,
    /// The fold refuses it (the last Owner, the founder, a linked computer
    /// or phone, older rules): why, for the person to read.
    Refused(String),
}

impl ChatService {
    /// This device's member leaving `workspace_id`: the event it would write,
    /// with its own heads as the watermark.
    fn leave_payload(&self, workspace_id: Uuid) -> Result<SessionEvent> {
        let me = self.author.id.clone();
        let watermark = self.cut_watermark(workspace_id, Some(&me), Some(self.device_id))?;
        Ok(SessionEvent::MemberLeft { member_id: me, account_id: self.author.account_id, watermark })
    }

    /// Whether this device's member asked to leave `workspace_id`.
    pub fn has_left(&self, workspace_id: Uuid) -> Result<bool> {
        let config = self.load_in(workspace_id, workspace_config_session_id(workspace_id))?;
        Ok(config.is_some_and(|c| c.left.contains_key(&self.author.id)))
    }

    /// Whether this device can leave `workspace_id` now (nothing written).
    pub fn leave_check(&mut self, workspace_id: Uuid) -> Result<LeaveCheck> {
        if self.has_left(workspace_id)? {
            return Ok(LeaveCheck::AlreadyLeft);
        }
        let payload = self.leave_payload(workspace_id)?;
        let cfg = workspace_config_session_id(workspace_id);
        Ok(match self.would_allow(cfg, workspace_id, payload.clone()) {
            Ok(()) => LeaveCheck::Ready,
            // The presence check runs first: judge the leave's own rules too,
            // so the founder or the last Owner never gets a passkey prompt
            // for a leave the fold would refuse anyway.
            Err(ChatError::Unauthorized(why)) if why == ligus_core::authz::PRESENCE_REQUIRED => {
                let env = self.draft_signed(cfg, workspace_id, payload)?;
                let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
                let Some(config) = config else { return Ok(LeaveCheck::Refused("this workspace hasn't synced yet".into())) };
                let SessionEvent::MemberLeft { member_id, account_id, .. } = &env.payload else { unreachable!("drafted a leave") };
                match ligus_core::authz::leave::leave_ok(&env, member_id, *account_id, true, timeline.current(), &config) {
                    Ok(()) => LeaveCheck::NeedsPasskey,
                    Err(why) => LeaveCheck::Refused(why),
                }
            }
            Err(ChatError::Unauthorized(why)) => LeaveCheck::Refused(why),
            Err(e) => return Err(e),
        })
    }

    /// Leave `workspace_id`: write `MemberLeft` on its config log (with this
    /// account's passkey when it holds one; the presence provider asks).
    /// Returns `false` when this member had already asked to leave. The
    /// caller pushes it to the relay and then forgets the connection.
    pub fn leave_workspace(&mut self, workspace_id: Uuid) -> Result<bool> {
        match self.leave_check(workspace_id)? {
            LeaveCheck::AlreadyLeft => return Ok(false),
            LeaveCheck::Refused(why) => return Err(ChatError::Unauthorized(why)),
            LeaveCheck::Ready | LeaveCheck::NeedsPasskey => {}
        }
        let payload = self.leave_payload(workspace_id)?;
        self.append_signed(workspace_config_session_id(workspace_id), workspace_id, payload)?;
        Ok(true)
    }
}
