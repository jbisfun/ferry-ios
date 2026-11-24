//! Apple ID sign-in (plume_core GrandSlam SRP + 2FA, anisette v3) and the
//! developer-services session xkit provisions with.
use std::{path::PathBuf, sync::Arc};

use futures::future::BoxFuture;
use plume_core::{
    AnisetteConfiguration, AnisetteProvider,
    auth::{Account, TwoFactorAction, TwoFactorMethod, TwoFactorRequest, anisette_data::AnisetteData},
};
use tokio::sync::Mutex;
use xkit::{
    DeveloperAPIAuthData, DeveloperAPIClient, DeveloperServicesClient, DeveloperServicesLoginToken, IdentifierPolicy,
    SigningContext, XcodeAuthData,
    developer_services::{ConfirmRevocation, Resource},
    grand_slam::{AnisetteDataProvider, XCODE_APP},
    utilities::{DirectoryStorage, KeyValueSigningInfoManager},
};

use crate::{
    Ctx, Error, Result, err,
    protocol::{AccountInfo, Config, DeveloperItem, Phone, PromptRequest},
};

pub struct Session {
    pub info: AccountInfo,
    pub context: SigningContext,
    pub manager: Arc<KeyValueSigningInfoManager>,
}

impl Session {
    pub fn client(&self) -> &DeveloperAPIClient {
        &self.context.developer_api_client
    }
}

pub fn anisette_config(cfg: &Config) -> AnisetteConfiguration {
    // Local: the Mac CoreADI library is fetched once from Apple's iTunes
    // installer into <data>/anisette/lib and run on-device.
    let provider = if cfg.anisette_mode == "remote" { AnisetteProvider::Remote } else { AnisetteProvider::Local };
    AnisetteConfiguration::new()
        .set_provider(provider)
        .set_anisette_url_v3(cfg.anisette_url.clone())
        // The anisette identity persists here; a trusted identity skips 2FA next time.
        .set_configuration_path(PathBuf::from(&cfg.data_dir).join("anisette"))
}

/// plume's anisette as xkit's provider.
struct SharedAnisette(Arc<Mutex<AnisetteData>>);
impl AnisetteDataProvider for SharedAnisette {
    fn headers(&self) -> BoxFuture<'_, xkit::Result<reqwest_headers::HeaderMap>> {
        Box::pin(async move {
            let mut data = self.0.lock().await;
            if data.needs_refresh() {
                *data = data.refresh().await.map_err(|e| xkit::Error::External(Box::new(e)))?;
            }
            let mut headers = reqwest_headers::HeaderMap::new();
            for (k, v) in &data.base_headers {
                if let (Ok(k), Ok(v)) = (
                    reqwest_headers::HeaderName::from_bytes(k.as_bytes()),
                    reqwest_headers::HeaderValue::from_str(v),
                ) {
                    headers.insert(k, v);
                }
            }
            Ok(headers)
        })
    }
}

use reqwest::header as reqwest_headers;

fn two_factor(ctx: &Ctx, req: TwoFactorRequest) -> std::result::Result<TwoFactorAction, String> {
    let method = if req.method == TwoFactorMethod::Sms { "sms" } else { "device" };
    let phones = req
        .trusted_phone_numbers
        .iter()
        .map(|p| Phone { id: p.id, number: p.number_with_dial_code.clone() })
        .collect();
    let answer = ctx
        .prompt_blocking(PromptRequest::TwoFactor { method: method.into(), phones })
        .map_err(|e| e.to_string())?;
    if let Some(code) = answer.code {
        return Ok(TwoFactorAction::SubmitCode(code.trim().to_owned()));
    }
    if let Some(id) = answer.sms {
        return Ok(TwoFactorAction::SendSms(id));
    }
    Err("Two-factor authentication was cancelled".into())
}

/// plume's login loop, but an unreadable `/auth` (HTTP 403 for some
/// identities) falls back to a push to trusted devices instead of failing:
/// Apple asks new identities for SMS 2FA, and the options request that lists
/// the phone numbers is the one it refuses.
async fn gsa_login(anisette: AnisetteData, email: &str, password: &str, ctx: &Ctx) -> std::result::Result<Account, plume_core::Error> {
    use plume_core::auth::LoginState;
    let mut acct = Account::new_with_anisette(anisette)?;
    let mut state = acct.login_email_pass(email, password).await?;
    for _ in 0..12 {
        log::info!("GSA state: {}", format!("{state:?}").split('(').next().unwrap_or_default());
        state = match state {
            LoginState::LoggedIn => return Ok(acct),
            LoginState::NeedsLogin => acct.login_email_pass(email, password).await?,
            LoginState::NeedsExtraStep(step) => {
                if acct.get_pet().is_some() {
                    return Ok(acct);
                }
                return Err(plume_core::Error::ExtraStep(step));
            }
            // 2FA exactly as SideStore's SideSign does it (its headers, its
            // endpoints); plume's Xcode 11.2 headers get 403s and pushes that
            // never arrive.
            // Needs2FAVerification = Apple already pushed a code (hsc 409).
            LoginState::NeedsDevice2FA => {
                second_factor(&acct, ctx, true, false).await?;
                LoginState::NeedsLogin
            }
            LoginState::Needs2FAVerification => {
                second_factor(&acct, ctx, true, true).await?;
                LoginState::NeedsLogin
            }
            LoginState::NeedsSMS2FA | LoginState::NeedsSMS2FAVerification(_) => {
                second_factor(&acct, ctx, false, false).await?;
                LoginState::NeedsLogin
            }
        };
    }
    Err(plume_core::Error::AuthSrpWithMessage(0, "Sign-in didn't finish".into()))
}

const PHONE_BASE: &str = "https://gsa.apple.com/auth/verify/phone";

/// Headers for Apple's /auth/verify endpoints, matching SideSign.
async fn verify_headers(acct: &Account) -> std::result::Result<reqwest::header::HeaderMap, plume_core::Error> {
    use base64::Engine;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let spd = acct.spd.as_ref().ok_or_else(|| plume_core::Error::AuthenticationResponse("Missing 2FA session".into()))?;
    let get = |k: &str| spd.get(k).and_then(plist::Value::as_string).unwrap_or_default().to_owned();
    let identity = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", get("adsid"), get("GsIdmsToken")));
    let mut h = HeaderMap::new();
    for (k, v) in acct.get_anisette().await?.generate_headers(false, true, false) {
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
            h.insert(k, v);
        }
    }
    for (k, v) in [
        ("Accept", "application/x-buddyml"),
        ("Accept-Language", "en-us"),
        ("Content-Type", "application/x-plist"),
        ("User-Agent", "Xcode"),
        // SideSign's makeTwoFactorAuthRequest set.
        ("X-Apple-App-Info", "com.apple.gs.xcode.auth"),
        ("X-Xcode-Version", "26.0"),
        ("Connection", "close"),
    ] {
        h.insert(k, HeaderValue::from_static(v));
    }
    h.insert("X-Apple-Identity-Token", HeaderValue::from_str(&identity).map_err(|_| plume_core::Error::AuthenticationResponse("Bad identity token".into()))?);
    Ok(h)
}

fn plist_body(v: plist::Value) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = plist::to_writer_xml(&mut out, &v);
    out
}

fn dict(pairs: &[(&str, plist::Value)]) -> plist::Value {
    plist::Value::Dictionary(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

/// `ec`/`em` from a plist or JSON reply (also under `Status`).
fn reply_error(body: &[u8]) -> (i64, Option<String>, Option<plist::Dictionary>) {
    let d: Option<plist::Dictionary> = plist::from_bytes(body).ok().or_else(|| serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|j| plist::to_value(&j).ok()).and_then(|v| v.into_dictionary()));
    let Some(d) = d else { return (0, None, None) };
    let status = d.get("Status").and_then(|s| s.as_dictionary());
    let ec = d.get("ec").or_else(|| status.and_then(|s| s.get("ec"))).and_then(|v| v.as_signed_integer()).unwrap_or(0);
    let em = d.get("em").or_else(|| status.and_then(|s| s.get("em"))).and_then(|v| v.as_string()).map(str::to_owned);
    (ec, em, Some(d))
}

/// Apple's reply, digits masked, for the log.
fn masked(body: &[u8]) -> String {
    String::from_utf8_lossy(body).chars().filter(|c| !c.is_control()).map(|c| if c.is_ascii_digit() { '#' } else { c }).take(600).collect()
}

/// Title/message of an XMLUI `<alert>` Apple returns instead of an error code.
fn xmlui_alert(body: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(body);
    if s.contains("<pinView") {
        return None;
    }
    let start = s.find("<alert")?;
    let tag = &s[start..start + s[start..].find('>')?];
    if tag.contains(" id=") {
        return None;
    }
    let attr = |name: &str| {
        let i = tag.find(&format!("{name}=\""))? + name.len() + 2;
        Some(tag[i..i + tag[i..].find('"')?].to_owned())
    };
    attr("message").or_else(|| attr("title")).or_else(|| Some("Apple refused the request".into()))
}

fn phones_from(d: &plist::Dictionary) -> Vec<plume_core::auth::TrustedPhoneNumber> {
    let list = d.get("trustedPhoneNumbers").or_else(|| d.get("phoneNumbers")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|p| {
            let p = p.as_dictionary()?;
            let id = p.get("id")?.as_signed_integer()? as u32;
            let number = ["numberWithDialCode", "obfuscatedNumber"]
                .iter()
                .find_map(|k| p.get(*k).and_then(|v| v.as_string()).map(str::to_owned))
                .or_else(|| p.get("lastTwoDigits").and_then(|v| v.as_string()).map(|d| format!("••{d}")))
                .unwrap_or_else(|| format!("Phone {id}"));
            serde_json::from_value(serde_json::json!({"id": id, "numberWithDialCode": number, "lastTwoDigits": "", "pushMode": "sms"})).ok()
        })
        .collect()
}

enum Channel {
    Device,
    Sms(String),
}

/// SideSign's handle2FARequest: learn the trusted numbers, deliver a code by
/// push or SMS, verify it. Apple wants another SRP login afterwards.
async fn second_factor(acct: &Account, ctx: &Ctx, trusted_device: bool, already_sent: bool) -> std::result::Result<(), plume_core::Error> {
    let fail = |m: String| plume_core::Error::AuthSrpWithMessage(0, m);
    let client = reqwest::Client::builder().build().map_err(|e| fail(e.to_string()))?;

    // The numbers Apple has (SideSign reads them from the login reply; /auth has them too).
    let mut phones = Vec::new();
    {
        let mut h = verify_headers(acct).await?;
        h.insert("Accept", reqwest::header::HeaderValue::from_static("application/json"));
        match client.get("https://gsa.apple.com/auth").headers(h).send().await {
            Ok(res) => {
                let status = res.status().as_u16();
                let bytes = res.bytes().await.unwrap_or_default();
                let (_, _, d) = reply_error(&bytes);
                if let Some(d) = d.as_ref() {
                    phones = phones_from(d);
                }
                log::info!("2FA options: HTTP {status}, {} trusted numbers", phones.len());
                if phones.is_empty() {
                    log::debug!("2FA options body: {}", masked(&bytes));
                }
            }
            Err(e) => log::warn!("2FA options: {e}"),
        }
    }

    let send = async |channel: &Channel| -> std::result::Result<Channel, plume_core::Error> {
        match channel {
            Channel::Device => {
                let res = client
                    .get("https://gsa.apple.com/auth/verify/trusteddevice")
                    .headers(verify_headers(acct).await?)
                    .send()
                    .await
                    .map_err(|e| fail(e.to_string()))?;
                let status = res.status().as_u16();
                let bytes = res.bytes().await.unwrap_or_default();
                log::info!("2FA push: HTTP {status}: {}", masked(&bytes));
                if let Some(alert) = xmlui_alert(&bytes) {
                    return Err(fail(alert));
                }
                if status != 200 {
                    return Err(plume_core::Error::AuthSrpWithMessage(status.into(), "Apple didn't send a code to your devices".into()));
                }
                Ok(Channel::Device)
            }
            Channel::Sms(id) => {
                let body = dict(&[("serverInfo", dict(&[("mode", "sms".into()), ("phoneNumber.id", id.clone().into())]))]);
                let res = client
                    .post(format!("{PHONE_BASE}/put?mode=sms"))
                    .headers(verify_headers(acct).await?)
                    .body(plist_body(body))
                    .send()
                    .await
                    .map_err(|e| fail(e.to_string()))?;
                let status = res.status().as_u16();
                let bytes = res.bytes().await.unwrap_or_default();
                let (ec, em, d) = reply_error(&bytes);
                log::info!("2FA SMS to phone {id}: HTTP {status} ec {ec}: {}", masked(&bytes));
                if let Some(alert) = xmlui_alert(&bytes) {
                    return Err(fail(alert));
                }
                if ec != 0 || status != 200 {
                    return Err(plume_core::Error::AuthSrpWithMessage(if ec != 0 { ec } else { status.into() }, em.unwrap_or_else(|| "Apple didn't send a text".into())));
                }
                let id = d
                    .as_ref()
                    .and_then(|d| d.get("phoneNumber"))
                    .and_then(|p| p.as_dictionary())
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_signed_integer().map(|i| i.to_string()).or_else(|| v.as_string().map(str::to_owned)))
                    .unwrap_or_else(|| id.clone());
                Ok(Channel::Sms(id))
            }
        }
    };

    let mut channel = if trusted_device {
        if already_sent { Channel::Device } else { send(&Channel::Device).await? }
    } else {
        send(&Channel::Sms(phones.first().map(|p| p.id.to_string()).unwrap_or_else(|| "1".into()))).await?
    };
    loop {
        let method = if matches!(channel, Channel::Device) { TwoFactorMethod::Device } else { TwoFactorMethod::Sms };
        let req = TwoFactorRequest { method, trusted_phone_numbers: phones.clone() };
        match tokio::task::block_in_place(|| two_factor(ctx, req)).map_err(fail)? {
            TwoFactorAction::SendSms(id) => channel = send(&Channel::Sms(id.to_string())).await?,
            TwoFactorAction::SubmitCode(code) => {
                let res = match &channel {
                    Channel::Device => {
                        let mut h = verify_headers(acct).await?;
                        h.insert("security-code", reqwest::header::HeaderValue::from_str(&code).map_err(|_| plume_core::Error::Bad2faCode)?);
                        client.get("https://gsa.apple.com/grandslam/GsService2/validate").headers(h).send().await
                    }
                    Channel::Sms(id) => {
                        let body = dict(&[
                            ("securityCode.code", code.clone().into()),
                            ("serverInfo", dict(&[("mode", "sms".into()), ("phoneNumber.id", id.clone().into())])),
                        ]);
                        client
                            .post(format!("{PHONE_BASE}/securitycode?referrer=/auth/verify/phone/put"))
                            .headers(verify_headers(acct).await?)
                            .body(plist_body(body))
                            .send()
                            .await
                    }
                }
                .map_err(|e| fail(e.to_string()))?;
                let status = res.status().as_u16();
                let pe = res.headers().contains_key("x-apple-pe-token");
                let bytes = res.bytes().await.unwrap_or_default();
                let (ec, em, _) = reply_error(&bytes);
                log::info!("2FA verify: HTTP {status} ec {ec} pe-token {pe}");
                if ec == -21669 || (ec == 0 && status != 200) || (matches!(channel, Channel::Sms(_)) && status == 200 && ec == 0 && !pe) {
                    // Wrong code: ask again without sending another.
                    continue;
                }
                if ec != 0 {
                    return Err(plume_core::Error::AuthSrpWithMessage(ec, em.unwrap_or_else(|| "Verification failed".into())));
                }
                if let Some(alert) = xmlui_alert(&bytes) {
                    return Err(fail(alert));
                }
                return Ok(());
            }
        }
    }
}

/// Forget the provisioned anisette identity but keep the downloaded CoreADI.
fn reset_identity(anisette_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(anisette_dir) else { return };
    for e in entries.flatten() {
        if e.file_name() == "lib" {
            continue;
        }
        let p = e.path();
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

pub async fn login(ctx: &Ctx, email: &str, password: &str) -> Result<AccountInfo> {
    let cfg = ctx.shared.config();
    ctx.progress("Signing in", None);
    let email_lc = email.trim().to_lowercase();
    let password = password.to_owned();
    let c2 = ctx.clone();
    let anisette_dir = PathBuf::from(&cfg.data_dir).join("anisette");
    // Identities provisioned before the client-info fix were made under the
    // Mac description Apple blocks, and GSA rejects them (-22410). Start over once.
    let marker = anisette_dir.join("client-info-v2");
    if !marker.exists() {
        reset_identity(&anisette_dir);
        let _ = std::fs::create_dir_all(&anisette_dir);
        let _ = std::fs::write(&marker, b"");
    }
    let mut attempt = 0;
    let account = loop {
        attempt += 1;
        let anisette = AnisetteData::new(anisette_config(&cfg)).await.map_err(|e| {
            log::warn!("anisette failed: {e:?}");
            e
        })?;
        // Shape only (no values): which headers, their lengths, and the client info.
        let mut shape: Vec<String> = anisette
            .base_headers
            .iter()
            .map(|(k, v)| if k.eq_ignore_ascii_case("x-mme-client-info") || k.eq_ignore_ascii_case("user-agent") { format!("{k}={v}") } else { format!("{k}[{}]", v.len()) })
            .collect();
        shape.sort();
        log::info!("anisette ({}): {}", cfg.anisette_mode, shape.join(", "));
        match gsa_login(anisette, &email_lc, &password, &c2).await {
            Ok(a) => break a,
            Err(e) => {
                log::warn!("GSA login failed: {e:?}");
                let rejected = matches!(e, plume_core::Error::AuthSrpWithMessage(-22410 | -22419, _));
                if rejected && attempt == 1 && cfg.anisette_mode != "remote" {
                    log::info!("Apple rejected this anisette identity; provisioning a new one");
                    ctx.progress("Setting up this iPhone with Apple", None);
                    reset_identity(&anisette_dir);
                    continue;
                }
                return Err(e.into());
            }
        }
    };

    ctx.progress("Loading team", None);
    let adsid = account
        .spd
        .as_ref()
        .and_then(|d| d.get("adsid"))
        .and_then(plist::Value::as_string)
        .ok_or_else(|| Error::Message("Apple didn't return an account id".into()))?
        .to_owned();
    let token = account.get_app_token(XCODE_APP).await?;
    let login_token = DeveloperServicesLoginToken {
        adsid,
        token: token.auth_token,
        // Apple rejects a stale token; we sign in again on every launch.
        expiry: chrono::Utc::now() + chrono::Duration::days(30),
    };
    let anisette: Arc<dyn AnisetteDataProvider> = Arc::new(SharedAnisette(account.anisette.clone()));
    let services = DeveloperServicesClient::new(login_token.clone(), anisette.clone())?;
    let teams = services.list_teams().await?;
    // Prefer a paid team when the Apple ID has both.
    let team = teams
        .iter()
        .find(|t| !t.is_free())
        .or(teams.first())
        .cloned()
        .ok_or_else(|| Error::Message("This Apple ID has no developer team. Sign in to developer.apple.com once to accept the agreement.".into()))?;

    let client = DeveloperAPIClient::new(
        DeveloperAPIAuthData::Xcode(XcodeAuthData { login_token, team_id: team.id.clone() }),
        Some(anisette),
    )?
    .with_identifier_policy(IdentifierPolicy::Verbatim);
    let manager = Arc::new(KeyValueSigningInfoManager {
        storage: Arc::new(DirectoryStorage::new(PathBuf::from(&cfg.data_dir).join("signing"))),
    });
    let (first, last) = account.get_name();
    let info = AccountInfo {
        email: email.trim().to_owned(),
        name: format!("{first} {last}").trim().to_owned(),
        team_id: team.id.clone(),
        team_name: team.name.clone(),
        free: team.is_free(),
    };
    let reply = AccountInfo { ..clone_info(&info) };
    *ctx.shared.session.lock().await = Some(Session { info, context: SigningContext::new(client, manager.clone()), manager });
    Ok(reply)
}

fn clone_info(i: &AccountInfo) -> AccountInfo {
    AccountInfo { email: i.email.clone(), name: i.name.clone(), team_id: i.team_id.clone(), team_name: i.team_name.clone(), free: i.free }
}

/// Asks the user before xkit revokes a development certificate (free
/// accounts get two; the one in use on another computer may be revoked).
pub fn confirm_revocation(ctx: &Ctx) -> Box<ConfirmRevocation> {
    let ctx = ctx.clone();
    Box::new(move |certs: Vec<Resource>| {
        let ctx = ctx.clone();
        Box::pin(async move {
            let names = certs.iter().map(|c| describe_certificate(c).name).collect();
            matches!(ctx.prompt(PromptRequest::RevokeCertificates { certificates: names }).await, Ok(a) if a.revoke == Some(true))
        })
    })
}

fn day(r: &Resource, key: &str) -> Option<String> {
    r.attribute(key).map(|s| s.chars().take(10).collect())
}

fn describe_certificate(r: &Resource) -> DeveloperItem {
    let machine = r.attribute("machineName").unwrap_or_default();
    DeveloperItem {
        id: r.id.clone(),
        name: r.attribute("displayName").or_else(|| r.attribute("name")).unwrap_or("Certificate").to_owned(),
        detail: if machine.is_empty() {
            r.attribute("certificateType").unwrap_or("DEVELOPMENT").to_owned()
        } else {
            format!("{} · {machine}", r.attribute("certificateType").unwrap_or("DEVELOPMENT"))
        },
        expires: day(r, "expirationDate"),
    }
}

async fn with_session<T>(ctx: &Ctx, f: impl AsyncFnOnce(&Session) -> Result<T>) -> Result<T> {
    let guard = ctx.shared.session.lock().await;
    let s = guard.as_ref().ok_or(Error::NotSignedIn)?;
    f(s).await
}

pub async fn certificates(ctx: &Ctx) -> Result<Vec<DeveloperItem>> {
    with_session(ctx, async |s| Ok(s.client().certificates().await?.iter().map(describe_certificate).collect())).await
}

pub async fn revoke_certificate(ctx: &Ctx, id: &str) -> Result<()> {
    with_session(ctx, async |s| Ok(s.client().revoke_certificate(id).await?)).await
}

pub async fn app_ids(ctx: &Ctx) -> Result<Vec<DeveloperItem>> {
    with_session(ctx, async |s| {
        Ok(s.client()
            .bundle_ids()
            .await?
            .iter()
            .map(|r| DeveloperItem {
                id: r.id.clone(),
                name: r.attribute("name").unwrap_or("App ID").to_owned(),
                detail: r.attribute("identifier").unwrap_or_default().to_owned(),
                expires: None,
            })
            .collect())
    })
    .await
}

pub async fn delete_app_id(ctx: &Ctx, id: &str) -> Result<()> {
    with_session(ctx, async |s| {
        s.client().delete_bundle_id(id).await.or_else(|e| {
            if e.to_string().contains("forbidden") || e.to_string().contains("403") {
                err("Free accounts can't delete App IDs; they expire after 7 days")
            } else {
                Err(e.into())
            }
        })
    })
    .await
}
