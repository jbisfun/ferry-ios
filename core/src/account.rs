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
    AnisetteConfiguration::new()
        .set_provider(AnisetteProvider::Remote)
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

pub async fn login(ctx: &Ctx, email: &str, password: &str) -> Result<AccountInfo> {
    let cfg = ctx.shared.config();
    ctx.progress("Signing in", None);
    let email_lc = email.trim().to_lowercase();
    let password = password.to_owned();
    let c2 = ctx.clone();
    let account = Account::login(move || Ok((email_lc.clone(), password.clone())), move |r| two_factor(&c2, r), anisette_config(&cfg)).await?;

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
