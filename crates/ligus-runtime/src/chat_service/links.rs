//! Linked identities on the config log (IDENTITY-AND-FORGES §2.2): drafting
//! the statement this account's forge key signs, publishing and removing a
//! link, and reading the links in force.

use std::collections::BTreeMap;

use ligus_core::identity_link::{self, IdentityLink, LinkMethod};
use ligus_core::{workspace_config_session_id, SessionEvent, SessionEventEnvelope, Timestamp};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// What a new link claims, before it is signed.
#[derive(Debug, Clone)]
pub struct LinkDraft {
    pub provider: String,
    pub host: String,
    pub subject_id: String,
    pub handle: String,
    pub method: LinkMethod,
    /// One link id for every workspace it's published in (so removing it is
    /// one id), a fresh nonce per statement.
    pub link_id: Uuid,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
}

impl LinkDraft {
    /// A draft issued now (whole seconds), lasting the default year.
    pub fn new(
        provider: &str,
        host: &str,
        subject_id: &str,
        handle: &str,
        method: LinkMethod,
    ) -> Self {
        let now = Timestamp::now()
            .0
            .replace_nanosecond(0)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        Self {
            provider: provider.trim().to_ascii_lowercase(),
            host: host.trim().to_ascii_lowercase(),
            subject_id: subject_id.trim().to_string(),
            handle: handle.trim().trim_start_matches('@').to_string(),
            method,
            link_id: Uuid::new_v4(),
            issued_at: Timestamp(now),
            expires_at: Timestamp(now + time::Duration::days(identity_link::DEFAULT_LIFETIME_DAYS)),
        }
    }
}

impl ChatService {
    fn link_account(&self) -> Result<Uuid> {
        if self.delegated {
            return Err(ChatError::Unauthorized(
                "this device acts for its owner without the account key; link identities from one of the owner's own devices"
                    .into(),
            ));
        }
        self.author
            .account_id
            .ok_or_else(|| ChatError::Unauthorized("this device has no account yet".into()))
    }

    /// The link (unsigned for `SshSig`: `proof` and `key_fingerprint` are
    /// filled once the forge key signed `statement`) this account would
    /// publish in `workspace_id`. The statement names this account, its key,
    /// the workspace, the forge and the stable subject id.
    pub fn draft_identity_link(&self, workspace_id: Uuid, d: &LinkDraft) -> Result<IdentityLink> {
        let account_id = self.link_account()?;
        let mut link = IdentityLink {
            link_id: d.link_id,
            account_id,
            provider: d.provider.clone(),
            host: d.host.clone(),
            subject_id: d.subject_id.clone(),
            handle_at_link: d.handle.clone(),
            method: d.method,
            statement: String::new(),
            proof: String::new(),
            key_fingerprint: String::new(),
            nonce: String::new(),
            issued_at: d.issued_at,
            expires_at: d.expires_at,
        };
        if d.method == LinkMethod::SshSig {
            link.nonce = identity_link::new_nonce();
            link.statement =
                identity_link::render_statement(&link, workspace_id, &self.account_public_key());
        }
        Ok(link)
    }

    /// Publish `link` on `workspace_id`'s config log, signed by this device.
    /// The fold checks it is this account's, well formed and (SSH-signed)
    /// that its statement names this workspace and the account key here.
    pub fn publish_identity_link(
        &mut self,
        workspace_id: Uuid,
        link: IdentityLink,
    ) -> Result<SessionEventEnvelope> {
        if self.local_workspace_id == Some(workspace_id) {
            return Err(ChatError::Unauthorized(
                "links are published to shared workspaces".into(),
            ));
        }
        let account = self.link_account()?;
        if link.account_id != account {
            return Err(ChatError::Unauthorized(
                "an identity link is published by its own account".into(),
            ));
        }
        identity_link::check_shape(&link).map_err(ChatError::Unauthorized)?;
        self.append_signed(
            workspace_config_session_id(workspace_id),
            workspace_id,
            SessionEvent::IdentityLinkPublished { link },
        )
    }

    /// Remove a link (this account's own, or another member's as an Admin).
    pub fn remove_identity_link(
        &mut self,
        workspace_id: Uuid,
        account_id: Uuid,
        link_id: Uuid,
    ) -> Result<SessionEventEnvelope> {
        if self.author.account_id == Some(account_id) {
            // Unlinking is the account's own act, like linking.
            self.link_account()?;
        }
        self.append_signed(
            workspace_config_session_id(workspace_id),
            workspace_id,
            SessionEvent::IdentityLinkRemoved {
                account_id,
                link_id,
            },
        )
    }

    /// The links in force at the head of `workspace_id`'s config log, by
    /// account, and each account's current (undisputed) key there.
    pub fn identity_links(&self, workspace_id: Uuid) -> Result<WorkspaceLinks> {
        let timeline = match self.pinned_config(workspace_id) {
            Ok((_, t)) => t,
            Err(_) => {
                self.store
                    .load_config(
                        workspace_id,
                        self.trusted_founder.as_deref(),
                        self.local_workspace_id,
                    )?
                    .1
            }
        };
        let head = timeline.current();
        Ok(WorkspaceLinks {
            links: head.links.clone(),
            account_keys: head.account_keys.clone(),
        })
    }

    /// A signed `IdentityLinkPublished` outside any log: the form a contact
    /// record carries (its workspace is the contact's). Opt-in per link.
    pub fn signed_link_publication(
        &self,
        contact_workspace_id: Uuid,
        link: IdentityLink,
    ) -> SessionEventEnvelope {
        let mut env = SessionEventEnvelope::new(
            contact_workspace_id,
            contact_workspace_id,
            1,
            SessionEvent::IdentityLinkPublished { link },
        );
        env.actor_stamp = Some(ligus_core::ActorStamp {
            actor: self.author.clone(),
            recorded_at: Timestamp::now(),
            cfg_frontier: None,
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: Default::default(),
            log_head: None,
        });
        ligus_core::crypto::sign_envelope(&mut env, self.device_id, &self.keypair);
        env
    }
}

/// The links in force in one workspace.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceLinks {
    pub links: BTreeMap<Uuid, BTreeMap<Uuid, IdentityLink>>,
    pub account_keys: BTreeMap<Uuid, Vec<u8>>,
}

impl WorkspaceLinks {
    pub fn of(&self, account: Uuid) -> Vec<&IdentityLink> {
        self.links
            .get(&account)
            .map(|m| m.values().collect())
            .unwrap_or_default()
    }
}

/// One identity link as this device shows it: the link, every workspace it
/// is published in, and the account key it is checked under (the first
/// workspace's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownLink {
    pub link: IdentityLink,
    pub workspaces: Vec<Uuid>,
    pub account_key: Vec<u8>,
}

/// The links `account` shows across the workspaces this device holds, only
/// from workspaces whose log registers the account key `accept_key` agrees
/// with (this device's own key for its account; the key the co-member
/// directory agrees on for anyone else). A workspace founded by someone
/// else can register any key under any account id, and links checked under
/// that key would show the founder's forge accounts as the account's.
/// Links are merged across workspaces by the identity they name (the link
/// id, forge, subject, method and signing key), never by the link id alone,
/// which the publisher chooses.
pub fn links_for_account(
    per_ws: &[(Uuid, WorkspaceLinks)],
    account: Uuid,
    accept_key: impl Fn(&[u8]) -> bool,
) -> Vec<ShownLink> {
    let same = |a: &IdentityLink, b: &IdentityLink| {
        a.link_id == b.link_id
            && a.subject_key() == b.subject_key()
            && a.method == b.method
            && a.key_fingerprint == b.key_fingerprint
    };
    let mut out: Vec<ShownLink> = Vec::new();
    for (ws, wl) in per_ws {
        let Some(key) = wl
            .account_keys
            .get(&account)
            .filter(|k| accept_key(k))
            .cloned()
        else {
            continue;
        };
        for l in wl.of(account) {
            match out.iter_mut().find(|s| same(&s.link, l)) {
                Some(s) if !s.workspaces.contains(ws) => s.workspaces.push(*ws),
                Some(_) => {}
                None => out.push(ShownLink {
                    link: l.clone(),
                    workspaces: vec![*ws],
                    account_key: key.clone(),
                }),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ligus_core::{ActorIdentity, ActorKind, SigningKeypair};
    use ssh_key::private::Ed25519Keypair;
    use ssh_key::{HashAlg, LineEnding, PrivateKey};

    use super::*;
    use crate::identity_links::{
        self, CachedCheck, LinkCheckCache, LinkContext, LinkStatus, LINK_NAMESPACE,
    };
    use crate::EventStore;

    fn founder() -> (ChatService, Uuid) {
        let store = EventStore::open_in_memory().unwrap();
        let account = Uuid::new_v4();
        let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
        author.account_id = Some(account);
        let mut svc = ChatService::new(
            store,
            SigningKeypair::generate().unwrap(),
            SigningKeypair::generate().unwrap(),
            author,
        );
        let wid = Uuid::new_v4();
        svc.ensure_workspace_config(wid).unwrap();
        (svc, wid)
    }

    fn sign(svc: &ChatService, wid: Uuid, forge_key: &PrivateKey) -> IdentityLink {
        let d = LinkDraft::new(
            "github",
            "github.com",
            "1234567",
            "@ana",
            LinkMethod::SshSig,
        );
        let mut l = svc.draft_identity_link(wid, &d).unwrap();
        l.proof = forge_key
            .sign(LINK_NAMESPACE, HashAlg::Sha512, l.statement.as_bytes())
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap();
        l.key_fingerprint = forge_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        l
    }

    #[test]
    fn publish_read_and_remove_through_the_service() {
        let (mut svc, wid) = founder();
        let k = PrivateKey::from(Ed25519Keypair::from_seed(&[1; 32]));
        let l = sign(&svc, wid, &k);
        assert_eq!(l.handle_at_link, "ana");
        assert!(l.statement.contains(&format!("workspace: {wid}")));
        svc.publish_identity_link(wid, l.clone()).unwrap();
        let links = svc.identity_links(wid).unwrap();
        let mine = links.of(svc.author().account_id.unwrap());
        assert_eq!(mine.len(), 1);
        // The published link passes the offline checks at the head.
        let key = links.account_keys.get(&l.account_id).cloned();
        let ctx = LinkContext {
            workspace_id: wid,
            account_key: key.as_deref(),
        };
        assert!(
            identity_links::offline_check(mine[0], ctx, Timestamp::now())
                .unwrap()
                .is_some()
        );
        // Another workspace's statement can't be published here.
        let other = Uuid::new_v4();
        svc.ensure_workspace_config(other).unwrap();
        assert!(
            svc.publish_identity_link(other, l.clone()).is_err(),
            "cross-workspace replay"
        );
        svc.remove_identity_link(wid, l.account_id, l.link_id)
            .unwrap();
        assert!(svc.identity_links(wid).unwrap().links.is_empty());
    }

    #[test]
    fn a_delegated_host_cannot_draft_or_publish() {
        let (mut svc, wid) = founder();
        let k = PrivateKey::from(Ed25519Keypair::from_seed(&[1; 32]));
        let l = sign(&svc, wid, &k);
        let owner = svc.author().clone();
        svc.act_as_owner_device(owner);
        assert!(svc.publish_identity_link(wid, l).is_err());
        let d = LinkDraft::new("github", "github.com", "1", "a", LinkMethod::SshSig);
        assert!(svc.draft_identity_link(wid, &d).is_err());
    }

    /// A workspace whose log registers another key for an account (an
    /// attacker-founded workspace can register any key under any account
    /// id) must not put links on that account's People entry: they would be
    /// checked under the attacker's key and could show the attacker's forge
    /// account as the victim's, verified. Links are also merged by the
    /// identity they name, not by the publisher-chosen link id.
    #[test]
    fn links_from_a_workspace_with_another_account_key_are_not_shown() {
        let victim = Uuid::new_v4();
        let (good, evil) = (vec![1u8; 32], vec![2u8; 32]);
        let link = |id: u128, subject: &str| {
            let d = LinkDraft::new("github", "github.com", subject, subject, LinkMethod::SshSig);
            IdentityLink {
                link_id: Uuid::from_u128(id),
                account_id: victim,
                provider: d.provider,
                host: d.host,
                subject_id: d.subject_id,
                handle_at_link: d.handle,
                method: d.method,
                statement: String::new(),
                proof: String::new(),
                key_fingerprint: String::new(),
                nonce: String::new(),
                issued_at: d.issued_at,
                expires_at: d.expires_at,
            }
        };
        let wl = |key: &Vec<u8>, links: Vec<IdentityLink>| WorkspaceLinks {
            links: BTreeMap::from([(victim, links.into_iter().map(|l| (l.link_id, l)).collect())]),
            account_keys: BTreeMap::from([(victim, key.clone())]),
        };
        let (w1, w2, w3, w4) = (
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            Uuid::from_u128(4),
        );
        let per_ws = vec![
            // The attacker's workspace first: its links must not win the merge.
            (w2, wl(&evil, vec![link(9, "666"), link(10, "667")])),
            (w1, wl(&good, vec![link(9, "100")])),
            (w3, wl(&good, vec![link(9, "100")])),
            // Same link id, another identity: shown on its own.
            (w4, wl(&good, vec![link(9, "200")])),
        ];
        let shown = links_for_account(&per_ws, victim, |k| k == good.as_slice());
        let got: Vec<(&str, Vec<Uuid>)> = shown
            .iter()
            .map(|s| (s.link.subject_id.as_str(), s.workspaces.clone()))
            .collect();
        assert_eq!(got, vec![("100", vec![w1, w3]), ("200", vec![w4])]);
        assert!(shown.iter().all(|s| s.account_key == good));
        assert!(
            links_for_account(&per_ws, victim, |_| false).is_empty(),
            "no agreed key: no links"
        );
    }

    #[test]
    fn link_checks_cache_in_the_event_store() {
        let store = EventStore::open_in_memory().unwrap();
        let c = CachedCheck {
            status: LinkStatus::SignatureOnly,
            checked_at: Timestamp::epoch(),
            last_verified_at: None,
        };
        assert!(store.get_check("k").is_none());
        store.put_check("k", &c);
        assert_eq!(store.get_check("k"), Some(c));
    }
}
