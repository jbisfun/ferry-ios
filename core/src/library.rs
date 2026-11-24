//! Apps Ferry installed, kept so they can be refreshed before their 7-day
//! (free) or 1-year (paid) profile runs out:
//!   <data>/library/<original id>/{app.ipa, record.json, icon.png}
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    Ctx, Result, device, err, install,
    protocol::{Config, LibraryApp, RefreshFailure, RefreshResult},
};

fn root(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.data_dir).join("library")
}

fn dir_for(cfg: &Config, original_id: &str) -> Result<PathBuf> {
    if original_id.is_empty() || original_id.contains('/') || original_id.starts_with('.') {
        return err(format!("Bad bundle id '{original_id}'"));
    }
    Ok(root(cfg).join(original_id))
}

pub fn save(cfg: &Config, ipa: &Path, icon: Option<&Path>, mut app: LibraryApp) -> Result<LibraryApp> {
    let dir = dir_for(cfg, &app.original_bundle_id)?;
    fs::create_dir_all(&dir)?;
    let kept = dir.join("app.ipa");
    let same = fs::canonicalize(ipa).ok() == fs::canonicalize(&kept).ok();
    if !same {
        // Copy to a temp name first so a failed copy never loses the old IPA.
        let tmp = dir.join("app.ipa.part");
        fs::copy(ipa, &tmp)?;
        fs::rename(&tmp, &kept)?;
    }
    if let Some(icon) = icon {
        let dest = dir.join("icon.png");
        if fs::copy(icon, &dest).is_ok() {
            app.icon_path = Some(dest.to_string_lossy().into_owned());
        }
    } else if dir.join("icon.png").exists() {
        app.icon_path = Some(dir.join("icon.png").to_string_lossy().into_owned());
    }
    fs::write(dir.join("record.json"), serde_json::to_vec_pretty(&app)?)?;
    Ok(app)
}

pub fn list(cfg: &Config) -> Result<Vec<LibraryApp>> {
    let mut out = vec![];
    let Ok(rd) = fs::read_dir(root(cfg)) else { return Ok(out) };
    for e in rd.flatten() {
        let Ok(bytes) = fs::read(e.path().join("record.json")) else { continue };
        if let Ok(app) = serde_json::from_slice::<LibraryApp>(&bytes) {
            out.push(app);
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

fn expires_within(app: &LibraryApp, days: i64) -> bool {
    match app.expires_at.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
        Some(t) => t.signed_duration_since(chrono::Utc::now()) < chrono::Duration::days(days),
        None => true,
    }
}

pub async fn refresh(ctx: &Ctx, ids: Vec<String>, within_days: i64) -> Result<RefreshResult> {
    let cfg = ctx.shared.config();
    let apps: Vec<LibraryApp> = list(&cfg)?
        .into_iter()
        .filter(|a| if ids.is_empty() { expires_within(a, within_days) } else { ids.contains(&a.original_bundle_id) })
        .collect();
    let mut result = RefreshResult { refreshed: vec![], failed: vec![] };
    let total = apps.len().max(1) as f64;
    for (i, app) in apps.into_iter().enumerate() {
        ctx.progress(&format!("Refreshing {}", app.name), Some(i as f64 / total));
        let ipa = dir_for(&cfg, &app.original_bundle_id)?.join("app.ipa");
        match install::install(ctx, &ipa, app.options.clone(), true).await {
            Ok(_) => result.refreshed.push(app.original_bundle_id),
            Err(e) => result.failed.push(RefreshFailure { bundle_id: app.original_bundle_id, message: e.to_string() }),
        }
    }
    Ok(result)
}

pub async fn remove(ctx: &Ctx, original_id: &str, uninstall: bool) -> Result<()> {
    let cfg = ctx.shared.config();
    let dir = dir_for(&cfg, original_id)?;
    if uninstall {
        if let Ok(bytes) = fs::read(dir.join("record.json")) {
            let app: LibraryApp = serde_json::from_slice(&bytes)?;
            device::uninstall(&cfg, &app.installed_bundle_id).await?;
        }
    }
    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::InstallOptions;

    fn cfg(dir: &Path) -> Config {
        Config {
            data_dir: dir.to_string_lossy().into(),
            tmp_dir: dir.join("tmp").to_string_lossy().into(),
            anisette_mode: "local".into(),
            anisette_url: String::new(),
            device_address: String::new(),
        }
    }

    fn record(id: &str, expires: Option<String>) -> LibraryApp {
        LibraryApp {
            original_bundle_id: id.into(),
            installed_bundle_id: format!("{id}.TEAM"),
            name: id.into(),
            version: "1".into(),
            icon_path: None,
            installed_at: chrono::Utc::now().to_rfc3339(),
            expires_at: expires,
            team_id: "TEAM".into(),
            granted_entitlements: serde_json::json!({}),
            options: InstallOptions::default(),
        }
    }

    #[test]
    fn save_list_and_expiry() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path());
        let ipa = d.path().join("x.ipa");
        fs::write(&ipa, b"PK").unwrap();
        let soon = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let later = (chrono::Utc::now() + chrono::Duration::days(6)).to_rfc3339();
        save(&c, &ipa, None, record("b.app", Some(later))).unwrap();
        let a = save(&c, &ipa, None, record("a.app", Some(soon))).unwrap();
        // Saving from the kept copy itself must not fail or truncate it.
        save(&c, &d.path().join("library/a.app/app.ipa"), None, a).unwrap();
        assert_eq!(fs::read(d.path().join("library/a.app/app.ipa")).unwrap(), b"PK");
        let l = list(&c).unwrap();
        assert_eq!(l.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["a.app", "b.app"]);
        assert!(expires_within(&l[0], 3));
        assert!(!expires_within(&l[1], 3));
        assert!(dir_for(&c, "../etc").is_err());
    }
}
