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
    let mut phones = Vec::new();
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
            LoginState::NeedsDevice2FA | LoginState::NeedsSMS2FA => match acct.get_auth_extras().await {
                Ok(extras) => {
                    phones = extras.trusted_phone_numbers.clone();
                    match extras.new_state {
                        Some(s) => s,
                        None if matches!(state, LoginState::NeedsSMS2FA) && !phones.is_empty() => {
                            acct.send_sms_2fa_to_devices(phones[0].id).await?
                        }
                        None => acct.send_2fa_to_devices().await?,
                    }
                }
                Err(e) => {
                    log::warn!("2FA options unavailable ({e:?}); pushing to trusted devices");
                    acct.send_2fa_to_devices().await?
                }
            },
            LoginState::Needs2FAVerification => {
                let req = TwoFactorRequest { method: TwoFactorMethod::Device, trusted_phone_numbers: phones.clone() };
                match tokio::task::block_in_place(|| two_factor(ctx, req)).map_err(|e| plume_core::Error::AuthSrpWithMessage(0, e))? {
                    TwoFactorAction::SubmitCode(code) => acct.verify_2fa(code).await?,
                    TwoFactorAction::SendSms(id) => acct.send_sms_2fa_to_devices(id).await?,
                }
            }
            LoginState::NeedsSMS2FAVerification(body) => {
                let req = TwoFactorRequest { method: TwoFactorMethod::Sms, trusted_phone_numbers: phones.clone() };
                match tokio::task::block_in_place(|| two_factor(ctx, req)).map_err(|e| plume_core::Error::AuthSrpWithMessage(0, e))? {
                    TwoFactorAction::SubmitCode(code) => acct.verify_sms_2fa(code, body).await?,
                    TwoFactorAction::SendSms(id) => acct.send_sms_2fa_to_devices(id).await?,
                }
            }
        };
    }
    Err(plume_core::Error::AuthSrpWithMessage(0, "Sign-in didn't finish".into()))
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
