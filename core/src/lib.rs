//! Ferry for iOS core. The Swift app talks to it through a tiny C ABI that
//! carries JSON (see `protocol`):
//!
//!   ferry_core_start(config_json, callback, ctx)   once, at launch
//!   ferry_core_request(request_json) -> id          result arrives as an event
//!   ferry_core_respond(prompt, answer_json)          answers a Prompt event
//!   ferry_core_cancel(id)
//!   ferry_core_free(ptr)                             frees strings we return
//!
//! Every event (results, progress, prompts, logs) goes through the one
//! callback, on a core thread; the app hops to the main actor itself.
pub mod account;
pub mod bundle;
pub mod device;
pub mod inspect;
pub mod install;
pub mod library;
pub mod protocol;

use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use protocol::{Config, Event, PromptAnswer, PromptRequest, Request};
use serde::Serialize;

pub type Callback = extern "C" fn(json: *const c_char, ctx: *mut c_void);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("Not signed in")]
    NotSignedIn,
    #[error("Cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Plist(#[from] plist::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Xkit(#[from] xkit::Error),
    #[error("{0}")]
    Plume(#[from] plume_core::Error),
    #[error("{}", device::describe(.0))]
    Device(#[from] idevice::IdeviceError),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Message(msg.into()))
}

/// State shared by every request.
pub struct Shared {
    pub config: RwLock<Config>,
    callback: Callback,
    ctx: usize,
    prompts: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<PromptAnswer>>>,
    next_prompt: AtomicU64,
    pub session: tokio::sync::Mutex<Option<account::Session>>,
    /// Serialises device-changing work (installs, refreshes).
    pub device_lock: tokio::sync::Mutex<()>,
}

impl Shared {
    fn emit(&self, event: &Event) {
        if let Ok(s) = serde_json::to_string(event) {
            if let Ok(c) = CString::new(s) {
                (self.callback)(c.as_ptr(), self.ctx as *mut c_void);
            }
        }
    }

    pub fn log(&self, level: &str, message: impl Into<String>) {
        self.emit(&Event::Log { level: level.into(), message: message.into() });
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }
}

/// Per-request handle: progress and prompts are tagged with the request id.
#[derive(Clone)]
pub struct Ctx {
    pub id: u64,
    pub shared: Arc<Shared>,
}

impl Ctx {
    pub fn progress(&self, stage: &str, fraction: Option<f64>) {
        self.shared.emit(&Event::Progress { id: self.id, stage: stage.into(), fraction });
    }

    fn open_prompt(&self, request: PromptRequest) -> tokio::sync::oneshot::Receiver<PromptAnswer> {
        let prompt = self.shared.next_prompt.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.shared.prompts.lock().unwrap().insert(prompt, tx);
        self.shared.emit(&Event::Prompt { id: self.id, prompt, request });
        rx
    }

    pub async fn prompt(&self, request: PromptRequest) -> Result<PromptAnswer> {
        let answer = self.open_prompt(request).await.map_err(|_| Error::Cancelled)?;
        if answer.cancel { Err(Error::Cancelled) } else { Ok(answer) }
    }

    /// For library callbacks that are synchronous but run on a runtime worker.
    pub fn prompt_blocking(&self, request: PromptRequest) -> Result<PromptAnswer> {
        let rx = self.open_prompt(request);
        let answer = tokio::task::block_in_place(|| rx.blocking_recv()).map_err(|_| Error::Cancelled)?;
        if answer.cancel { Err(Error::Cancelled) } else { Ok(answer) }
    }
}

struct Core {
    runtime: tokio::runtime::Runtime,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    tasks: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
}

static CORE: OnceLock<Core> = OnceLock::new();

async fn dispatch(ctx: &Ctx, request: Request) -> Result<serde_json::Value> {
    fn to<T: Serialize>(v: T) -> Result<serde_json::Value> {
        Ok(serde_json::to_value(v)?)
    }
    match request {
        Request::SettingsUpdate(u) => {
            let mut c = ctx.shared.config.write().unwrap();
            if let Some(a) = u.anisette_url {
                c.anisette_url = a;
            }
            if let Some(a) = u.device_address {
                c.device_address = a;
            }
            to(&*c)
        }
        Request::AccountLogin { email, password } => to(account::login(ctx, &email, &password).await?),
        Request::AccountLogout => {
            *ctx.shared.session.lock().await = None;
            to(())
        }
        Request::AccountCertificates => to(account::certificates(ctx).await?),
        Request::AccountRevokeCertificate { id } => to(account::revoke_certificate(ctx, &id).await?),
        Request::AccountAppIds => to(account::app_ids(ctx).await?),
        Request::AccountDeleteAppId { id } => to(account::delete_app_id(ctx, &id).await?),
        Request::DeviceConnect => to(device::info(&ctx.shared.config()).await?),
        Request::DeviceApps => to(device::apps(&ctx.shared.config()).await?),
        Request::DeviceUninstall { bundle_id } => to(device::uninstall(&ctx.shared.config(), &bundle_id).await?),
        Request::IpaInspect { path } => {
            let cfg = ctx.shared.config();
            to(tokio::task::spawn_blocking(move || inspect::inspect(&cfg, std::path::Path::new(&path)))
                .await
                .map_err(|e| Error::Message(e.to_string()))??)
        }
        Request::AppInstall { ipa, options } => to(install::install(ctx, std::path::Path::new(&ipa), options, true).await?),
        Request::LibraryList => to(library::list(&ctx.shared.config())?),
        Request::LibraryRefresh { bundle_ids, within_days } => to(library::refresh(ctx, bundle_ids, within_days).await?),
        Request::LibraryRemove { bundle_id, uninstall } => to(library::remove(ctx, &bundle_id, uninstall).await?),
    }
}

fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok().map(str::to_owned)
}

/// Start the core. Returns false if the config is invalid or it already runs.
#[unsafe(no_mangle)]
pub extern "C" fn ferry_core_start(config_json: *const c_char, callback: Callback, ctx: *mut c_void) -> bool {
    let Some(config) = cstr(config_json).and_then(|s| serde_json::from_str::<Config>(&s).ok()) else {
        return false;
    };
    // reqwest/tungstenite can't pick a TLS crypto provider on their own here.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = std::fs::create_dir_all(&config.data_dir);
    let _ = std::fs::create_dir_all(&config.tmp_dir);
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build() else {
        return false;
    };
    let shared = Arc::new(Shared {
        config: RwLock::new(config),
        callback,
        ctx: ctx as usize,
        prompts: Mutex::new(HashMap::new()),
        next_prompt: AtomicU64::new(0),
        session: tokio::sync::Mutex::new(None),
        device_lock: tokio::sync::Mutex::new(()),
    });
    CORE.set(Core { runtime, shared, next_id: AtomicU64::new(0), tasks: Mutex::new(HashMap::new()) }).is_ok()
}

/// Queue a request; returns its id (0 if the core isn't started). A bad
/// request still gets an id and an Error event.
#[unsafe(no_mangle)]
pub extern "C" fn ferry_core_request(request_json: *const c_char) -> u64 {
    let Some(core) = CORE.get() else { return 0 };
    let id = core.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let ctx = Ctx { id, shared: core.shared.clone() };
    let text = cstr(request_json).unwrap_or_default();
    let handle = core.runtime.spawn(async move {
        let outcome = match serde_json::from_str::<Request>(&text) {
            Ok(request) => dispatch(&ctx, request).await,
            Err(e) => Err(Error::Message(format!("Bad request: {e}"))),
        };
        let event = match outcome {
            Ok(result) => Event::Result { id, result },
            Err(e) => Event::Error { id, message: e.to_string() },
        };
        ctx.shared.emit(&event);
        if let Some(core) = CORE.get() {
            core.tasks.lock().unwrap().remove(&id);
        }
    });
    core.tasks.lock().unwrap().insert(id, handle.abort_handle());
    id
}

#[unsafe(no_mangle)]
pub extern "C" fn ferry_core_respond(prompt: u64, answer_json: *const c_char) -> bool {
    let Some(core) = CORE.get() else { return false };
    let answer = cstr(answer_json)
        .and_then(|s| serde_json::from_str::<PromptAnswer>(&s).ok())
        .unwrap_or(PromptAnswer { code: None, sms: None, revoke: None, cancel: true });
    match core.shared.prompts.lock().unwrap().remove(&prompt) {
        Some(tx) => tx.send(answer).is_ok(),
        None => false,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ferry_core_cancel(id: u64) {
    let Some(core) = CORE.get() else { return };
    if let Some(h) = core.tasks.lock().unwrap().remove(&id) {
        h.abort();
        core.shared.emit(&Event::Error { id, message: Error::Cancelled.to_string() });
    }
}

/// JSON array of method names (for the app's protocol mirror test).
#[unsafe(no_mangle)]
pub extern "C" fn ferry_core_methods() -> *mut c_char {
    CString::new(serde_json::to_string(protocol::METHODS).unwrap()).unwrap().into_raw()
}

#[unsafe(no_mangle)]
pub extern "C" fn ferry_core_free(p: *mut c_char) {
    if !p.is_null() {
        drop(unsafe { CString::from_raw(p) });
    }
}
