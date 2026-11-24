//! Every request, result and event between the Swift app and the core, declared
//! once. The Swift side mirrors these as Codable types (App/Sources/Core/Protocol.swift);
//! `ferry_core_schema()` returns a description used by a test there.
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// Settings the app passes at start and may change later.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    /// Persistent data: library, xkit signing info, anisette state.
    pub data_dir: String,
    /// Scratch space, emptied by the app at launch.
    pub tmp_dir: String,
    /// "local": compute anisette on this iPhone (Apple's Mac CoreADI under an
    /// interpreter-only Unicorn, no JIT, no server). "remote": ask `anisette_url`.
    #[serde(default = "default_anisette_mode")]
    pub anisette_mode: String,
    /// Anisette v3 server, for "remote".
    #[serde(default = "default_anisette")]
    pub anisette_url: String,
    /// Where the loopback VPN (LocalDevVPN / StosVPN) reaches this device; "auto" = the tunnel peer.
    #[serde(default = "default_device_address")]
    pub device_address: String,
}

pub fn default_anisette_mode() -> String {
    "local".into()
}
pub fn default_anisette() -> String {
    "https://ani.sidestore.zip".into()
}
/// "auto" = the loopback VPN tunnel's peer address (10.7.0.1 if none is found).
pub fn default_device_address() -> String {
    "auto".into()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "settings.update")]
    SettingsUpdate(SettingsUpdate),

    #[serde(rename = "account.login")]
    AccountLogin { email: String, password: String },
    #[serde(rename = "account.logout")]
    AccountLogout,
    #[serde(rename = "account.certificates")]
    AccountCertificates,
    #[serde(rename = "account.revoke_certificate")]
    AccountRevokeCertificate { id: String },
    #[serde(rename = "account.app_ids")]
    AccountAppIds,
    #[serde(rename = "account.delete_app_id")]
    AccountDeleteAppId { id: String },

    #[serde(rename = "device.connect")]
    DeviceConnect,
    #[serde(rename = "device.apps")]
    DeviceApps,
    #[serde(rename = "device.uninstall")]
    DeviceUninstall { bundle_id: String },

    #[serde(rename = "ipa.inspect")]
    IpaInspect { path: String },

    #[serde(rename = "app.install")]
    AppInstall {
        ipa: String,
        #[serde(default)]
        options: InstallOptions,
    },

    #[serde(rename = "library.list")]
    LibraryList,
    #[serde(rename = "library.refresh")]
    LibraryRefresh {
        /// Original bundle ids; empty = everything expiring within `within_days`.
        #[serde(default)]
        bundle_ids: Vec<String>,
        #[serde(default = "default_within")]
        within_days: i64,
    },
    #[serde(rename = "library.remove")]
    LibraryRemove { bundle_id: String, uninstall: bool },
}

fn default_within() -> i64 {
    3
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SettingsUpdate {
    pub anisette_mode: Option<String>,
    pub anisette_url: Option<String>,
    pub device_address: Option<String>,
}

/// How an IPA is changed before signing.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct InstallOptions {
    /// CFBundleDisplayName override.
    pub display_name: Option<String>,
    /// Bundle id before the `.<TEAMID>` suffix; default = the app's own.
    pub bundle_id: Option<String>,
    /// CFBundleShortVersionString override.
    pub version: Option<String>,
    /// Drop PlugIns/ and Extensions/ (each extension costs a free-account App ID).
    pub remove_extensions: bool,
    /// Dylibs / .frameworks to copy into Frameworks/ and load from the main executable.
    pub inject: Vec<String>,
    /// Load commands (as listed by ipa.inspect) to remove from the main executable.
    pub remove_load_commands: Vec<String>,
    /// Allow iPad-only / file-sharing tweaks.
    pub enable_file_sharing: bool,
    pub force_all_devices: bool,
    /// Lower MinimumOSVersion, for apps that run fine on older iOS.
    pub minimum_os: Option<String>,
}

// ---- results --------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AccountInfo {
    pub email: String,
    pub name: String,
    pub team_id: String,
    pub team_name: String,
    /// Free ("Personal Team") accounts are capped at 3 apps / 10 App IDs per week.
    pub free: bool,
}

#[derive(Debug, Serialize)]
pub struct DeveloperItem {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub expires: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub udid: String,
    pub name: String,
    pub product_type: String,
    pub os_version: String,
}

#[derive(Debug, Serialize)]
pub struct InstalledApp {
    pub bundle_id: String,
    pub name: String,
    pub version: String,
    /// Signed by a development profile (i.e. sideloaded, expires).
    pub sideloaded: bool,
}

#[derive(Debug, Serialize)]
pub struct BinaryInfo {
    /// Path inside the .app, e.g. "Frameworks/Foo.framework/Foo".
    pub path: String,
    pub kind: String,
    pub entitlements: Json,
    pub load_commands: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct IpaInfo {
    pub name: String,
    pub bundle_id: String,
    pub version: String,
    pub build: String,
    pub minimum_os: String,
    /// Extracted icon (may be Apple's CgBI PNG, which UIImage decodes).
    pub icon_path: Option<String>,
    pub size: u64,
    pub binaries: Vec<BinaryInfo>,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LibraryApp {
    pub original_bundle_id: String,
    pub installed_bundle_id: String,
    pub name: String,
    pub version: String,
    pub icon_path: Option<String>,
    pub installed_at: String,
    pub expires_at: Option<String>,
    pub team_id: String,
    /// Entitlements the signed main executable actually got.
    pub granted_entitlements: Json,
    pub options: InstallOptions,
}

#[derive(Debug, Serialize)]
pub struct RefreshResult {
    pub refreshed: Vec<String>,
    pub failed: Vec<RefreshFailure>,
}

#[derive(Debug, Serialize)]
pub struct RefreshFailure {
    pub bundle_id: String,
    pub message: String,
}

// ---- events -----------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Result { id: u64, result: Json },
    Error { id: u64, message: String },
    Progress { id: u64, stage: String, fraction: Option<f64> },
    /// The app must answer with `ferry_core_respond(prompt, json)`.
    Prompt { id: u64, prompt: u64, request: PromptRequest },
    Log { level: String, message: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptRequest {
    /// Answer `{"code": "123456"}` or `{"sms": <phone id>}` or `{"cancel": true}`.
    TwoFactor { method: String, phones: Vec<Phone> },
    /// The iPhone wants to pair again and shows a code; answer `{"code": "123456"}`.
    PairingPin,
    /// Free accounts may hold 2 dev certificates; answer `{"revoke": true|false}`.
    RevokeCertificates { certificates: Vec<String> },
}

#[derive(Debug, Serialize)]
pub struct Phone {
    pub id: u32,
    pub number: String,
}

#[derive(Debug, Deserialize)]
pub struct PromptAnswer {
    pub code: Option<String>,
    pub sms: Option<u32>,
    pub revoke: Option<bool>,
    #[serde(default)]
    pub cancel: bool,
}

/// Method names, for the Swift-side mirror test.
pub const METHODS: &[&str] = &[
    "settings.update",
    "account.login",
    "account.logout",
    "account.certificates",
    "account.revoke_certificate",
    "account.app_ids",
    "account.delete_app_id",
    "device.connect",
    "device.apps",
    "device.uninstall",
    "ipa.inspect",
    "app.install",
    "library.list",
    "library.refresh",
    "library.remove",
];

/// plist -> JSON for display (data as base64-ish hex length, dates as RFC 3339).
pub fn plist_to_json(v: &plist::Value) -> Json {
    use plist::Value as P;
    match v {
        P::Array(a) => Json::Array(a.iter().map(plist_to_json).collect()),
        P::Dictionary(d) => Json::Object(d.iter().map(|(k, v)| (k.clone(), plist_to_json(v))).collect()),
        P::Boolean(b) => Json::Bool(*b),
        P::Data(d) => Json::String(format!("<{} bytes>", d.len())),
        P::Date(d) => Json::String(d.to_xml_format()),
        P::Real(r) => serde_json::Number::from_f64(*r).map(Json::Number).unwrap_or(Json::Null),
        P::Integer(i) => i
            .as_signed()
            .map(|n| Json::from(n))
            .or_else(|| i.as_unsigned().map(Json::from))
            .unwrap_or(Json::Null),
        P::String(s) => Json::String(s.clone()),
        P::Uid(u) => Json::from(u.get()),
        _ => Json::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_by_method_name() {
        let r: Request = serde_json::from_str(r#"{"method":"app.install","params":{"ipa":"/a.ipa","options":{"inject":["/x.dylib"],"remove_extensions":true}}}"#).unwrap();
        match r {
            Request::AppInstall { ipa, options } => {
                assert_eq!(ipa, "/a.ipa");
                assert_eq!(options.inject, vec!["/x.dylib"]);
                assert!(options.remove_extensions);
            }
            _ => panic!(),
        }
        let r: Request = serde_json::from_str(r#"{"method":"device.connect"}"#).unwrap();
        assert!(matches!(r, Request::DeviceConnect));
    }

    #[test]
    fn events_are_tagged() {
        let e = serde_json::to_string(&Event::Progress { id: 3, stage: "Signing".into(), fraction: Some(0.5) }).unwrap();
        assert_eq!(e, r#"{"type":"progress","id":3,"stage":"Signing","fraction":0.5}"#);
    }
}
