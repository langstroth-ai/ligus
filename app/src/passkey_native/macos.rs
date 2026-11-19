//! macOS: AuthenticationServices platform passkeys
//! (`ASAuthorizationPlatformPublicKeyCredentialProvider`) through `objc2`.
//!
//! The controller runs on the main thread (AppKit), with the main window as
//! its presentation anchor; the ceremony's thread waits on a channel. The
//! system builds `clientDataJSON` with origin `https://<rp id>`, and serves
//! only the RPs whose `apple-app-site-association` names this app
//! (`H5H3NNR456.ai.ligus.desktop`) and that the signed app's entitlements
//! list. An unsigned, unentitled or unassociated build gets an error back,
//! which falls back to the browser hand-off.
//!
//! Never exercised by the test suite (it would show the system dialog); see
//! `docs-site/docs/operate/security/native-passkeys.md` for the manual test.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use ligus_core::presence::native::{RawAssertion, RawRegistration};
use ligus_core::presence::PresencePolicy;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{available, define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_authentication_services::{
    ASAuthorization, ASAuthorizationController, ASAuthorizationControllerDelegate,
    ASAuthorizationControllerPresentationContextProviding, ASAuthorizationError, ASAuthorizationErrorDomain,
    ASAuthorizationPlatformPublicKeyCredentialAssertion, ASAuthorizationPlatformPublicKeyCredentialDescriptor,
    ASAuthorizationPlatformPublicKeyCredentialProvider, ASAuthorizationPlatformPublicKeyCredentialRegistration,
    ASAuthorizationPublicKeyCredentialAssertion, ASAuthorizationPublicKeyCredentialAssertionRequest,
    ASAuthorizationPublicKeyCredentialAttestationKindNone, ASAuthorizationPublicKeyCredentialRegistration,
    ASAuthorizationPublicKeyCredentialRegistrationRequest, ASAuthorizationPublicKeyCredentialUserVerificationPreferenceRequired,
    ASAuthorizationRequest, ASPublicKeyCredential,
};
use objc2_foundation::{NSArray, NSData, NSError, NSObject, NSObjectProtocol, NSString};
use tauri::Manager;

use super::{CreateRequest, GetRequest, NativeAuthenticator, NativeError};

/// The entitlements the app is signed with (the `webcredentials:` RPs).
const ENTITLEMENTS: &str = include_str!("../../Ligus.entitlements");

/// How long the main thread has to present the dialog, and to answer a
/// heartbeat while it is up ([`super::await_main_thread`]); the most a
/// ceremony's thread waits for an answer (the ceremony itself times out
/// earlier and cancels).
const WATCH: super::MainThreadWatch = super::MainThreadWatch {
    start_wait: Duration::from_secs(3),
    stall: Duration::from_secs(5),
    beat_every: Duration::from_secs(1),
    total: Duration::from_secs(600),
};

enum Credential {
    Get(RawAssertion),
    Create(RawRegistration),
}

type Reply = Result<Credential, NativeError>;

struct Ivars {
    reply: RefCell<Option<mpsc::Sender<Reply>>>,
    anchor: Retained<NSObject>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl ASAuthorizationControllerDelegate for Delegate {
        #[unsafe(method(authorizationController:didCompleteWithAuthorization:))]
        fn did_complete(&self, _controller: &ASAuthorizationController, authorization: &ASAuthorization) {
            self.send(read_credential(authorization));
        }

        #[unsafe(method(authorizationController:didCompleteWithError:))]
        fn did_fail(&self, _controller: &ASAuthorizationController, error: &NSError) {
            self.send(Err(map_error(error)));
        }
    }

    unsafe impl ASAuthorizationControllerPresentationContextProviding for Delegate {
        #[unsafe(method_id(presentationAnchorForAuthorizationController:))]
        fn anchor(&self, _controller: &ASAuthorizationController) -> Retained<NSObject> {
            self.ivars().anchor.clone()
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, reply: mpsc::Sender<Reply>, anchor: Retained<NSObject>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { reply: RefCell::new(Some(reply)), anchor });
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }

    /// Answer once; a second callback (none is expected) is dropped.
    fn send(&self, r: Reply) {
        if let Some(tx) = self.ivars().reply.borrow_mut().take() {
            let _ = tx.send(r);
        }
    }
}

thread_local! {
    /// The pending ceremony (main thread only): the controller holds its
    /// delegate weakly, so both are kept here until the next one.
    static PENDING: RefCell<Option<(Retained<ASAuthorizationController>, Retained<Delegate>)>> = const { RefCell::new(None) };
}

fn read_credential(authorization: &ASAuthorization) -> Reply {
    // SAFETY: plain property reads of a completed authorization.
    unsafe {
        let cred = authorization.credential();
        let any: &AnyObject = (*cred).as_ref();
        if let Some(a) = any.downcast_ref::<ASAuthorizationPlatformPublicKeyCredentialAssertion>() {
            return Ok(Credential::Get(RawAssertion {
                credential_id: a.credentialID().to_vec(),
                authenticator_data: a.rawAuthenticatorData().to_vec(),
                client_data_json: a.rawClientDataJSON().to_vec(),
                signature: a.signature().to_vec(),
            }));
        }
        if let Some(r) = any.downcast_ref::<ASAuthorizationPlatformPublicKeyCredentialRegistration>() {
            let Some(att) = r.rawAttestationObject() else {
                return Err(NativeError::Failed("the passkey came back without an attestation object".into()));
            };
            return Ok(Credential::Create(RawRegistration {
                credential_id: r.credentialID().to_vec(),
                attestation_object: att.to_vec(),
                client_data_json: r.rawClientDataJSON().to_vec(),
            }));
        }
    }
    Err(NativeError::Failed("an unexpected credential type".into()))
}

fn map_error(error: &NSError) -> NativeError {
    let msg = error.localizedDescription().to_string();
    // SAFETY: reading an extern static NSString.
    let ours = unsafe { error.domain().isEqualToString(ASAuthorizationErrorDomain) };
    if !ours {
        return NativeError::Failed(msg);
    }
    match ASAuthorizationError(error.code()) {
        ASAuthorizationError::Canceled => NativeError::Cancelled,
        // This authenticator already holds one of the account's passkeys.
        ASAuthorizationError::MatchedExcludedCredential => NativeError::Refused("this device already has one of your Ligus passkeys".into()),
        // Failed covers "not associated with the domain" (an unsigned or
        // unentitled build, a missing AASA entry), NotHandled no provider,
        // DeviceNotConfigured no iCloud Keychain: all go to the browser.
        _ => NativeError::Unavailable(msg),
    }
}

enum Op {
    Get(GetRequest),
    Create(CreateRequest),
}

/// Build and present the request on the main thread.
fn start(mtm: MainThreadMarker, op: Op, anchor: Retained<NSObject>, tx: mpsc::Sender<Reply>) {
    // SAFETY: AuthenticationServices calls on the main thread with live
    // arguments; the extern statics are Apple's constant strings.
    unsafe {
        let (Some(uv_required), Some(attestation_none)) =
            (ASAuthorizationPublicKeyCredentialUserVerificationPreferenceRequired, ASAuthorizationPublicKeyCredentialAttestationKindNone)
        else {
            let _ = tx.send(Err(NativeError::Unavailable("AuthenticationServices constants missing".into())));
            return;
        };
        let rp = match &op {
            Op::Get(g) => g.rp_id.clone(),
            Op::Create(c) => c.rp_id.clone(),
        };
        let provider = ASAuthorizationPlatformPublicKeyCredentialProvider::initWithRelyingPartyIdentifier(
            ASAuthorizationPlatformPublicKeyCredentialProvider::alloc(),
            &NSString::from_str(&rp),
        );
        let request: Retained<ASAuthorizationRequest> = match &op {
            Op::Get(g) => {
                let r = provider.createCredentialAssertionRequestWithChallenge(&NSData::with_bytes(&g.challenge));
                if !g.allow.is_empty() {
                    let descs: Vec<_> = g.allow.iter().map(|id| descriptor(id)).collect();
                    r.setAllowedCredentials(&NSArray::from_retained_slice(&descs));
                }
                r.setUserVerificationPreference(uv_required);
                Retained::into_super(r)
            }
            Op::Create(c) => {
                let r = provider.createCredentialRegistrationRequestWithChallenge_name_userID(
                    &NSData::with_bytes(&c.challenge),
                    &NSString::from_str(&c.user_name),
                    &NSData::with_bytes(&c.user_id),
                );
                r.setUserVerificationPreference(uv_required);
                r.setAttestationPreference(attestation_none);
                if !c.exclude.is_empty() {
                    // `excludedCredentials` is macOS 14.4+. Without it the
                    // system would silently replace an existing passkey of
                    // the account (same user handle): use the browser, which
                    // honours excludeCredentials.
                    let can: bool = msg_send![&*r, respondsToSelector: sel!(setExcludedCredentials:)];
                    if !can {
                        let _ = tx.send(Err(NativeError::Unavailable("this macOS can't exclude existing passkeys".into())));
                        return;
                    }
                    let descs: Vec<_> = c.exclude.iter().map(|id| descriptor(id)).collect();
                    let list = NSArray::from_retained_slice(&descs);
                    let _: () = msg_send![&*r, setExcludedCredentials: &*list];
                }
                Retained::into_super(r)
            }
        };
        let requests = NSArray::from_retained_slice(&[request]);
        let controller = ASAuthorizationController::initWithAuthorizationRequests(ASAuthorizationController::alloc(), &requests);
        let delegate = Delegate::new(mtm, tx, anchor);
        controller.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        controller.setPresentationContextProvider(Some(ProtocolObject::from_ref(&*delegate)));
        controller.performRequests();
        PENDING.with(|p| *p.borrow_mut() = Some((controller, delegate)));
    }
}

fn descriptor(id: &[u8]) -> Retained<ASAuthorizationPlatformPublicKeyCredentialDescriptor> {
    // SAFETY: a plain initializer.
    unsafe {
        ASAuthorizationPlatformPublicKeyCredentialDescriptor::initWithCredentialID(
            ASAuthorizationPlatformPublicKeyCredentialDescriptor::alloc(),
            &NSData::with_bytes(id),
        )
    }
}

pub struct MacAuthenticator {
    app: tauri::AppHandle,
    domains: Vec<String>,
}

impl MacAuthenticator {
    /// macOS 13 or later (the controller's `cancel`); `None` before.
    pub fn new(app: tauri::AppHandle) -> Option<Self> {
        if !available!(macos = 13.0) {
            return None;
        }
        Some(Self { app, domains: super::webcredential_domains(ENTITLEMENTS) })
    }

    fn run(&self, op: Op) -> Reply {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(AtomicU8::new(super::QUEUED));
        let app = self.app.clone();
        let st = state.clone();
        let reply = tx.clone();
        let queued = self.app.run_on_main_thread(move || {
            // The waiter may have given up (the main thread was busy): then
            // nothing is presented.
            if st.compare_exchange(super::QUEUED, super::STARTED, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                return;
            }
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = reply.send(Err(NativeError::Unavailable("not on the main thread".into())));
                return;
            };
            let window = app.get_webview_window("main").and_then(|w| w.ns_window().ok());
            // SAFETY: Tauri's NSWindow pointer for a live window, retained here.
            let anchor = window.and_then(|p| unsafe { Retained::retain(p.cast::<NSObject>()) });
            let Some(anchor) = anchor else {
                let _ = reply.send(Err(NativeError::Unavailable("no window to present the passkey dialog on".into())));
                return;
            };
            // A ceremony left over (its waiter gave up) ends now.
            PENDING.with(|p| {
                if let Some((c, _)) = p.borrow_mut().take() {
                    // SAFETY: main thread.
                    unsafe { c.cancel() };
                }
            });
            start(mtm, op, anchor, reply);
        });
        if let Err(e) = queued {
            return Err(NativeError::Unavailable(e.to_string()));
        }
        drop(tx);
        let app = self.app.clone();
        let on_main = move |f: Box<dyn FnOnce() + Send>| app.run_on_main_thread(f).is_ok();
        super::await_main_thread(&rx, &state, &WATCH, &on_main, &|| self.cancel())
    }
}

impl NativeAuthenticator for MacAuthenticator {
    fn origin(&self, policy: &PresencePolicy) -> Option<String> {
        if !self.domains.contains(&policy.rp_id) {
            return None;
        }
        policy.apple_native_origin()
    }

    fn get(&self, req: &GetRequest) -> Result<RawAssertion, NativeError> {
        match self.run(Op::Get(req.clone()))? {
            Credential::Get(a) => Ok(a),
            Credential::Create(_) => Err(NativeError::Failed("a registration answered a sign-in".into())),
        }
    }

    fn create(&self, req: &CreateRequest) -> Result<RawRegistration, NativeError> {
        match self.run(Op::Create(req.clone()))? {
            Credential::Create(r) => Ok(r),
            Credential::Get(_) => Err(NativeError::Failed("a sign-in answered a registration".into())),
        }
    }

    fn cancel(&self) {
        let _ = self.app.run_on_main_thread(|| {
            PENDING.with(|p| {
                if let Some((c, _)) = p.borrow().as_ref() {
                    // SAFETY: main thread; cancelling an idle or finished
                    // controller is a no-op.
                    unsafe { c.cancel() };
                }
            });
        });
    }
}
