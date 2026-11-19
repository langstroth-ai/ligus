//! Windows: platform passkeys through `webauthn.dll` (Windows Hello, security
//! keys, phones), loaded at run time so a Windows without it still starts
//! (and uses the browser hand-off).
//!
//! Windows lets a desktop app name any RP id, and the app builds the
//! `clientDataJSON` itself ([`client_data_json`], origin: the workspace's
//! hand-off page origin), so every well-formed workspace RP is served.
//! Options match the hand-off page: user verification required,
//! attestation "none", ES256 or EdDSA, the account's credentials allowed
//! (get) or excluded (create).
//!
//! Never exercised by the test suite (it would show the Windows Hello
//! dialog); see `docs-site/docs/operate/security/native-passkeys.md`.

use std::sync::{Mutex, OnceLock};

use hive_core::presence::native::{RawAssertion, RawRegistration};
use hive_core::presence::{client_data_json, PresencePolicy, EDDSA, ES256};
use tauri::Manager;
use windows_sys::core::{GUID, HRESULT, PCWSTR};
use windows_sys::Win32::Foundation::{HWND, NTE_EXISTS, NTE_USER_CANCELLED};
use windows_sys::Win32::Networking::WindowsWebServices::*;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

use super::{CreateRequest, GetRequest, NativeAuthenticator, NativeError};

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`.
const HR_CANCELLED: HRESULT = 0x800704C7_u32 as HRESULT;
/// How long Windows waits for the gesture (the ceremony cancels earlier).
const TIMEOUT_MS: u32 = 180_000;

/// What `GetProcAddress` returns.
type Proc = unsafe extern "system" fn() -> isize;
type FnApiVersion = unsafe extern "system" fn() -> u32;
type FnGetAssertion = unsafe extern "system" fn(
    HWND,
    PCWSTR,
    *const WEBAUTHN_CLIENT_DATA,
    *const WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
    *mut *mut WEBAUTHN_ASSERTION,
) -> HRESULT;
type FnMakeCredential = unsafe extern "system" fn(
    HWND,
    *const WEBAUTHN_RP_ENTITY_INFORMATION,
    *const WEBAUTHN_USER_ENTITY_INFORMATION,
    *const WEBAUTHN_COSE_CREDENTIAL_PARAMETERS,
    *const WEBAUTHN_CLIENT_DATA,
    *const WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS,
    *mut *mut WEBAUTHN_CREDENTIAL_ATTESTATION,
) -> HRESULT;
type FnFreeAssertion = unsafe extern "system" fn(*const WEBAUTHN_ASSERTION);
type FnFreeAttestation = unsafe extern "system" fn(*const WEBAUTHN_CREDENTIAL_ATTESTATION);
type FnCancellationId = unsafe extern "system" fn(*mut GUID) -> HRESULT;
type FnCancel = unsafe extern "system" fn(*const GUID) -> HRESULT;

/// The `webauthn.dll` entry points Ligus uses.
struct Api {
    get_assertion: FnGetAssertion,
    make_credential: FnMakeCredential,
    free_assertion: FnFreeAssertion,
    free_attestation: FnFreeAttestation,
    cancellation_id: FnCancellationId,
    cancel: FnCancel,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Api {
    /// Load `webauthn.dll` from System32 (never the app or current
    /// directory). `None` when it's missing or older than API version 1.
    fn load() -> Option<Self> {
        // SAFETY: loading a system DLL by name from System32 and reading
        // exports whose signatures are webauthn.h's.
        unsafe {
            let name = wide("webauthn.dll");
            let lib = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if lib.is_null() {
                return None;
            }
            let sym = |n: &[u8]| GetProcAddress(lib, n.as_ptr());
            let version = std::mem::transmute::<Proc, FnApiVersion>(sym(b"WebAuthNGetApiVersionNumber\0")?);
            if version() < WEBAUTHN_API_VERSION_1 {
                return None;
            }
            Some(Self {
                get_assertion: std::mem::transmute::<Proc, FnGetAssertion>(sym(b"WebAuthNAuthenticatorGetAssertion\0")?),
                make_credential: std::mem::transmute::<Proc, FnMakeCredential>(sym(b"WebAuthNAuthenticatorMakeCredential\0")?),
                free_assertion: std::mem::transmute::<Proc, FnFreeAssertion>(sym(b"WebAuthNFreeAssertion\0")?),
                free_attestation: std::mem::transmute::<Proc, FnFreeAttestation>(sym(b"WebAuthNFreeCredentialAttestation\0")?),
                cancellation_id: std::mem::transmute::<Proc, FnCancellationId>(sym(b"WebAuthNGetCancellationId\0")?),
                cancel: std::mem::transmute::<Proc, FnCancel>(sym(b"WebAuthNCancelCurrentOperation\0")?),
            })
        }
    }
}

fn map_hr(hr: HRESULT) -> NativeError {
    match hr {
        NTE_USER_CANCELLED | HR_CANCELLED => NativeError::Cancelled,
        // A credential on the exclude list is already on this authenticator.
        NTE_EXISTS => NativeError::Refused("this device already has one of your Ligus passkeys".into()),
        _ => NativeError::Failed(format!("Windows passkey error 0x{:08X}", hr as u32)),
    }
}

/// # Safety
/// `p` points at `n` readable bytes (or `n` is 0).
unsafe fn bytes(p: *const u8, n: u32) -> Vec<u8> {
    if p.is_null() || n == 0 {
        return Vec::new();
    }
    // SAFETY: per the contract.
    unsafe { std::slice::from_raw_parts(p, n as usize).to_vec() }
}

/// Credential descriptors for `ids` (they borrow `ids`).
fn credentials(ids: &[Vec<u8>]) -> Vec<WEBAUTHN_CREDENTIAL> {
    ids.iter()
        .map(|id| WEBAUTHN_CREDENTIAL {
            dwVersion: WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
            cbId: id.len() as u32,
            pbId: id.as_ptr() as *mut u8,
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
        })
        .collect()
}

/// The main window's handle (`HWND` as an integer), set at startup.
static MAIN_HWND: OnceLock<isize> = OnceLock::new();

/// Remember the main window for the passkey dialogs. Call on the main
/// thread (Tauri's setup), where reading the handle doesn't wait.
pub fn remember_window(app: &tauri::AppHandle) {
    if let Some(h) = app.get_webview_window("main").and_then(|w| w.hwnd().ok()) {
        let _ = MAIN_HWND.set(h.0 as isize);
    }
}

pub struct WinAuthenticator {
    api: Api,
    /// The pending operation's cancellation id.
    pending: Mutex<Option<GUID>>,
}

// SAFETY: `Api` holds only function pointers into a DLL that stays loaded
// for the life of the process; `GUID` is plain data.
unsafe impl Send for WinAuthenticator {}
unsafe impl Sync for WinAuthenticator {}

impl WinAuthenticator {
    pub fn load() -> Option<Self> {
        Some(Self { api: Api::load()?, pending: Mutex::new(None) })
    }

    /// The main window, read once on the main thread ([`remember_window`]):
    /// asking Tauri for it here would wait for the main thread, which may be
    /// blocked on the service lock this ceremony's caller holds.
    fn hwnd(&self) -> Result<HWND, NativeError> {
        match MAIN_HWND.get() {
            Some(&h) if h != 0 => Ok(h as HWND),
            _ => Err(NativeError::Unavailable("no window to present the passkey dialog on".into())),
        }
    }

    /// A fresh cancellation id, remembered for [`NativeAuthenticator::cancel`].
    fn begin(&self) -> GUID {
        let mut id: GUID = GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] };
        // SAFETY: an out pointer to a GUID.
        let _ = unsafe { (self.api.cancellation_id)(&mut id) };
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
        id
    }

    fn end(&self) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl NativeAuthenticator for WinAuthenticator {
    fn origin(&self, policy: &PresencePolicy) -> Option<String> {
        Some(policy.page_origin())
    }

    fn get(&self, req: &GetRequest) -> Result<RawAssertion, NativeError> {
        let client_data = client_data_json("webauthn.get", &req.challenge, &req.origin).into_bytes();
        let rp = wide(&req.rp_id);
        let mut allow = credentials(&req.allow);
        let hwnd = self.hwnd()?;
        let mut cancel_id = self.begin();
        // SAFETY: every pointer below borrows a local that outlives the call;
        // the options struct is zeroed, then the version-4 fields are set.
        let out = unsafe {
            let cd = WEBAUTHN_CLIENT_DATA {
                dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
                cbClientDataJSON: client_data.len() as u32,
                pbClientDataJSON: client_data.as_ptr() as *mut u8,
                pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
            };
            let mut opts: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS = std::mem::zeroed();
            opts.dwVersion = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_4;
            opts.dwTimeoutMilliseconds = TIMEOUT_MS;
            opts.CredentialList = WEBAUTHN_CREDENTIALS { cCredentials: allow.len() as u32, pCredentials: allow.as_mut_ptr() };
            opts.dwAuthenticatorAttachment = WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY;
            opts.dwUserVerificationRequirement = WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED;
            opts.pCancellationId = &mut cancel_id;
            let mut result: *mut WEBAUTHN_ASSERTION = std::ptr::null_mut();
            let hr = (self.api.get_assertion)(hwnd, rp.as_ptr(), &cd, &opts, &mut result);
            if hr < 0 || result.is_null() {
                Err(map_hr(hr))
            } else {
                let a = &*result;
                let raw = RawAssertion {
                    credential_id: bytes(a.Credential.pbId, a.Credential.cbId),
                    authenticator_data: bytes(a.pbAuthenticatorData, a.cbAuthenticatorData),
                    client_data_json: client_data.clone(),
                    signature: bytes(a.pbSignature, a.cbSignature),
                };
                (self.api.free_assertion)(result);
                Ok(raw)
            }
        };
        self.end();
        out
    }

    fn create(&self, req: &CreateRequest) -> Result<RawRegistration, NativeError> {
        let client_data = client_data_json("webauthn.create", &req.challenge, &req.origin).into_bytes();
        let rp_id = wide(&req.rp_id);
        let rp_name = wide("Ligus");
        let user_name = wide(&req.user_name);
        let mut exclude = credentials(&req.exclude);
        let mut params = [ES256, EDDSA].map(|alg| WEBAUTHN_COSE_CREDENTIAL_PARAMETER {
            dwVersion: WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
            lAlg: alg,
        });
        let hwnd = self.hwnd()?;
        let mut cancel_id = self.begin();
        // SAFETY: as in `get`; the options are zeroed, then the version-3
        // fields are set.
        let out = unsafe {
            let rp = WEBAUTHN_RP_ENTITY_INFORMATION {
                dwVersion: WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION,
                pwszId: rp_id.as_ptr(),
                pwszName: rp_name.as_ptr(),
                pwszIcon: std::ptr::null(),
            };
            let user = WEBAUTHN_USER_ENTITY_INFORMATION {
                dwVersion: WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
                cbId: req.user_id.len() as u32,
                pbId: req.user_id.as_ptr() as *mut u8,
                pwszName: user_name.as_ptr(),
                pwszIcon: std::ptr::null(),
                pwszDisplayName: user_name.as_ptr(),
            };
            let cose = WEBAUTHN_COSE_CREDENTIAL_PARAMETERS { cCredentialParameters: params.len() as u32, pCredentialParameters: params.as_mut_ptr() };
            let cd = WEBAUTHN_CLIENT_DATA {
                dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
                cbClientDataJSON: client_data.len() as u32,
                pbClientDataJSON: client_data.as_ptr() as *mut u8,
                pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
            };
            let mut opts: WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS = std::mem::zeroed();
            opts.dwVersion = WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_3;
            opts.dwTimeoutMilliseconds = TIMEOUT_MS;
            opts.CredentialList = WEBAUTHN_CREDENTIALS { cCredentials: exclude.len() as u32, pCredentials: exclude.as_mut_ptr() };
            opts.dwAuthenticatorAttachment = WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY;
            opts.dwUserVerificationRequirement = WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED;
            opts.dwAttestationConveyancePreference = WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE;
            opts.pCancellationId = &mut cancel_id;
            let mut result: *mut WEBAUTHN_CREDENTIAL_ATTESTATION = std::ptr::null_mut();
            let hr = (self.api.make_credential)(hwnd, &rp, &user, &cose, &cd, &opts, &mut result);
            if hr < 0 || result.is_null() {
                Err(map_hr(hr))
            } else {
                let a = &*result;
                let raw = RawRegistration {
                    credential_id: bytes(a.pbCredentialId, a.cbCredentialId),
                    attestation_object: bytes(a.pbAttestationObject, a.cbAttestationObject),
                    client_data_json: client_data.clone(),
                };
                (self.api.free_attestation)(result);
                if raw.attestation_object.is_empty() {
                    Err(NativeError::Failed("Windows returned no attestation object".into()))
                } else {
                    Ok(raw)
                }
            }
        };
        self.end();
        out
    }

    fn cancel(&self) {
        if let Some(id) = *self.pending.lock().unwrap_or_else(|e| e.into_inner()) {
            // SAFETY: a GUID by pointer; cancelling a finished operation is
            // an error code, ignored.
            let _ = unsafe { (self.api.cancel)(&id) };
        }
    }
}
