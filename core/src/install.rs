//! The whole sideload: unpack → edit → provision → sign → package → install.
use std::{
    fs,
    path::{Path, PathBuf},
};

use plist::Value;
use plume_core::MachO;
use xkit::{
    AutoSigner, TargetDevice,
    installation::{AppInstaller, ConnectionPreferences, InstallationStage},
    utilities::ZIPCompressor,
};

use crate::{
    Ctx, Error, Result, account, bundle, device, err, library,
    protocol::{InstallOptions, LibraryApp, plist_to_json},
};

/// Removes the staging directory however the install ends.
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// ExpirationDate of a .mobileprovision (the XML plist inside the CMS blob).
pub fn profile_expiry(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    let start = data.windows(5).position(|w| w == b"<?xml")?;
    let end = data.windows(8).rposition(|w| w == b"</plist>")? + 8;
    let d: plist::Dictionary = plist::from_bytes(&data[start..end]).ok()?;
    match d.get("ExpirationDate")? {
        Value::Date(date) => Some(date.to_xml_format()),
        _ => None,
    }
}

fn friendly(e: Error) -> Error {
    let s = e.to_string();
    let l = s.to_lowercase();
    if l.contains("maximum number of installed apps") || l.contains("applicationverificationfailed") && l.contains("limit") {
        Error::Message("Free Apple IDs can have 3 sideloaded apps at once (Ferry counts as one). Remove one in My Apps and try again.".into())
    } else if l.contains("maximum app id") || l.contains("app id limit") || l.contains("cannot create more than 10") {
        Error::Message("Free Apple IDs can register 10 App IDs per 7 days. Wait for some to expire (Settings → Account → App IDs), or reuse a bundle id.".into())
    } else {
        e
    }
}

fn icon_in(app: &Path, info: &plist::Dictionary) -> Option<PathBuf> {
    let mut names: Vec<String> = vec![];
    if let Some(files) = info
        .get("CFBundleIcons")
        .and_then(Value::as_dictionary)
        .and_then(|d| d.get("CFBundlePrimaryIcon"))
        .and_then(Value::as_dictionary)
        .and_then(|d| d.get("CFBundleIconFiles"))
        .and_then(Value::as_array)
    {
        names.extend(files.iter().rev().filter_map(Value::as_string).map(str::to_owned));
    }
    names.push("AppIcon".into());
    for want in names {
        let best = fs::read_dir(app)
            .ok()?
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with(&want) && n.ends_with(".png")
            })
            .max_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0));
        if let Some(e) = best {
            return Some(e.path());
        }
    }
    None
}

pub async fn install(ctx: &Ctx, ipa: &Path, opts: InstallOptions, record: bool) -> Result<LibraryApp> {
    install_inner(ctx, ipa, opts, record).await.map_err(friendly)
}

async fn install_inner(ctx: &Ctx, ipa: &Path, opts: InstallOptions, record: bool) -> Result<LibraryApp> {
    let cfg = ctx.shared.config();
    let (mut context, team_id) = {
        let guard = ctx.shared.session.lock().await;
        let s = guard.as_ref().ok_or(Error::NotSignedIn)?;
        (s.context.clone(), s.info.team_id.clone())
    };
    let _device = ctx.shared.device_lock.lock().await;

    ctx.progress("Connecting", None);
    let conn = device::connection(&cfg).await?;
    let device_name = conn.device_name().await.unwrap_or_else(|_| "iPhone".into());
    let udid = conn.udid.clone();

    ctx.progress("Unpacking", Some(0.0));
    let staging = Staging(PathBuf::from(&cfg.tmp_dir).join(format!("install-{}", uuid::Uuid::new_v4())));
    {
        let (ipa, dir, c) = (ipa.to_owned(), staging.0.clone(), ctx.clone());
        tokio::task::spawn_blocking(move || ZIPCompressor::decompress(&ipa, &dir, &|f| c.progress("Unpacking", f)))
            .await
            .map_err(|e| Error::Message(e.to_string()))??;
    }
    let payload = staging.0.join("Payload");
    let app = bundle::find_app(&payload)?;

    ctx.progress("Preparing", None);
    let exe_name = bundle::string(&bundle::read_info(&app)?, "CFBundleExecutable");
    let existing = bundle::load_commands(&app.join(&exe_name));
    let (original_id, installed_id) = bundle::prepare(&app, &opts, &team_id, &|lc| existing.iter().any(|e| e == lc))?;
    let info = bundle::read_info(&app)?;

    context.target_device = Some(TargetDevice { udid: udid.clone(), name: device_name });
    let confirm = account::confirm_revocation(ctx);
    let stage = std::sync::Mutex::new(String::from("Provisioning"));
    AutoSigner { context }
        .sign(
            &app,
            confirm.as_ref(),
            &|s| {
                *stage.lock().unwrap() = s.to_owned();
                ctx.progress(s, None);
            },
            &|f| ctx.progress(&stage.lock().unwrap().clone(), f),
            &|| Ok(()),
        )
        .await?;

    let granted = MachO::new(app.join(&exe_name))
        .ok()
        .and_then(|m| m.entitlements().clone())
        .map(|d| plist_to_json(&Value::Dictionary(d)))
        .unwrap_or_default();
    let expires_at = profile_expiry(&app.join("embedded.mobileprovision"));
    let icon = icon_in(&app, &info);

    let signed = staging.0.join("signed.ipa");
    {
        let (payload, out, c) = (payload.clone(), signed.clone(), ctx.clone());
        tokio::task::spawn_blocking(move || ZIPCompressor::compress(&payload, &out, &|f| c.progress("Packaging", f)))
            .await
            .map_err(|e| Error::Message(e.to_string()))??;
    }

    AppInstaller { ipa: signed, udid, connection_preferences: ConnectionPreferences::default() }
        .install_with_connection(&conn, &|st: InstallationStage| ctx.progress(st.display_name(), Some(st.display_progress())))
        .await?;

    let app_record = LibraryApp {
        original_bundle_id: original_id,
        installed_bundle_id: installed_id,
        name: [bundle::string(&info, "CFBundleDisplayName"), bundle::string(&info, "CFBundleName")]
            .into_iter()
            .find(|n| !n.is_empty())
            .unwrap_or_else(|| exe_name.clone()),
        version: bundle::string(&info, "CFBundleShortVersionString"),
        icon_path: None,
        installed_at: chrono::Utc::now().to_rfc3339(),
        expires_at,
        team_id,
        granted_entitlements: granted,
        options: opts,
    };
    if !record {
        return Ok(app_record);
    }
    if ipa.extension().is_none_or(|e| !e.eq_ignore_ascii_case("ipa")) {
        return err("Only .ipa files can be kept for refreshing");
    }
    library::save(&cfg, ipa, icon.as_deref(), app_record)
}
