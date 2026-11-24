//! Edits to an unpacked .app before it is signed: identifiers, Info.plist
//! tweaks, extension removal and dylib injection.
use std::{
    fs,
    path::{Path, PathBuf},
};

use plist::{Dictionary, Value};
use plume_core::MachO;

use crate::{Result, err, protocol::InstallOptions};

pub fn find_app(payload: &Path) -> Result<PathBuf> {
    let mut apps = vec![];
    for e in fs::read_dir(payload)? {
        let p = e?.path();
        if p.is_dir() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("app")) {
            apps.push(p);
        }
    }
    match apps.len() {
        1 => Ok(apps.remove(0)),
        0 => err("This IPA has no app in Payload/"),
        _ => err("This IPA has more than one app in Payload/"),
    }
}

pub fn read_info(bundle: &Path) -> Result<Dictionary> {
    Ok(plist::from_file(bundle.join("Info.plist"))?)
}

fn write_info(bundle: &Path, d: &Dictionary) -> Result<()> {
    plist::to_file_binary(bundle.join("Info.plist"), d)?;
    Ok(())
}

pub fn string(d: &Dictionary, k: &str) -> String {
    d.get(k).and_then(Value::as_string).unwrap_or_default().to_owned()
}

/// Every nested bundle with its own Info.plist (extensions, watch apps, App Clips).
pub fn nested_bundles(app: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
            if matches!(ext, "appex" | "app") && p.join("Info.plist").exists() {
                out.push(p.clone());
            }
            walk(&p, out);
        }
    }
    for sub in ["PlugIns", "Extensions", "Watch", "AppClips"] {
        walk(&app.join(sub), &mut out);
    }
    out
}

/// The installed bundle id: `<base>.<TEAMID>`, the AltStore/SideStore
/// convention that keeps free-account App IDs unique per team.
pub fn installed_bundle_id(base: &str, team_id: &str) -> String {
    if base.ends_with(&format!(".{team_id}")) { base.to_owned() } else { format!("{base}.{team_id}") }
}

/// Apply `opts` to the unpacked app; returns (original id, installed id).
pub fn prepare(app: &Path, opts: &InstallOptions, team_id: &str, frameworks_in: &dyn Fn(&str) -> bool) -> Result<(String, String)> {
    let mut info = read_info(app)?;
    let original = string(&info, "CFBundleIdentifier");
    if original.is_empty() {
        return err("The app has no CFBundleIdentifier");
    }
    let base = opts.bundle_id.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| original.clone());
    let new_id = installed_bundle_id(base.trim(), team_id);

    if opts.remove_extensions {
        for sub in ["PlugIns", "Extensions", "Watch", "AppClips"] {
            let _ = fs::remove_dir_all(app.join(sub));
        }
    }

    // Nested bundles keep their suffix under the new prefix.
    for nested in nested_bundles(app) {
        let mut d = read_info(&nested)?;
        let id = string(&d, "CFBundleIdentifier");
        let replaced = match id.strip_prefix(&original) {
            Some(rest) => format!("{new_id}{rest}"),
            None => format!("{new_id}.{}", id.rsplit('.').next().unwrap_or("ext")),
        };
        d.insert("CFBundleIdentifier".into(), replaced.into());
        if let Some(Value::Dictionary(ext)) = d.get_mut("NSExtension") {
            if let Some(Value::Dictionary(attrs)) = ext.get_mut("NSExtensionAttributes") {
                if let Some(Value::String(w)) = attrs.get_mut("WKAppBundleIdentifier") {
                    *w = w.replacen(&original, &new_id, 1);
                }
            }
        }
        if let Some(Value::String(c)) = d.get_mut("WKCompanionAppBundleIdentifier") {
            *c = c.replacen(&original, &new_id, 1);
        }
        write_info(&nested, &d)?;
    }

    info.insert("CFBundleIdentifier".into(), new_id.clone().into());
    if let Some(n) = opts.display_name.as_ref().filter(|s| !s.trim().is_empty()) {
        info.insert("CFBundleDisplayName".into(), n.trim().into());
    }
    if let Some(v) = opts.version.as_ref().filter(|s| !s.trim().is_empty()) {
        info.insert("CFBundleShortVersionString".into(), v.trim().into());
    }
    if let Some(m) = opts.minimum_os.as_ref().filter(|s| !s.trim().is_empty()) {
        info.insert("MinimumOSVersion".into(), m.trim().into());
    }
    if opts.enable_file_sharing {
        info.insert("UIFileSharingEnabled".into(), true.into());
        info.insert("UISupportsDocumentBrowser".into(), true.into());
    }
    if opts.force_all_devices {
        info.insert("UIDeviceFamily".into(), Value::Array(vec![1.into(), 2.into()]));
        info.remove("UIRequiredDeviceCapabilities");
    }
    // Apps that check their own receipt or bundle id can read the original.
    info.insert("FerryOriginalBundleIdentifier".into(), original.clone().into());
    write_info(app, &info)?;

    let exe = app.join(string(&info, "CFBundleExecutable"));
    if !opts.remove_load_commands.is_empty() {
        let mut m = MachO::new(&exe)?;
        for lc in &opts.remove_load_commands {
            m.remove_dylib(lc)?;
        }
    }
    if !opts.inject.is_empty() {
        inject(app, &exe, &opts.inject, frameworks_in)?;
    }
    Ok((original, new_id))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// Copy each dylib/framework into Frameworks/ and add a weak load command for
/// it to the main executable (plus an @executable_path/Frameworks rpath).
fn inject(app: &Path, exe: &Path, items: &[String], already_loaded: &dyn Fn(&str) -> bool) -> Result<()> {
    let frameworks = app.join("Frameworks");
    fs::create_dir_all(&frameworks)?;
    let mut m = MachO::new(exe)?;
    if !has_rpath(exe, "@executable_path/Frameworks")? {
        m.add_rpath("@executable_path/Frameworks")?;
    }
    for item in items {
        let src = Path::new(item);
        let name = src.file_name().and_then(|n| n.to_str()).ok_or_else(|| crate::Error::Message(format!("Bad path {item}")))?;
        let load = if src.is_dir() && name.ends_with(".framework") {
            copy_dir(src, &frameworks.join(name))?;
            format!("@rpath/{name}/{}", name.trim_end_matches(".framework"))
        } else if name.ends_with(".dylib") {
            fs::copy(src, frameworks.join(name))?;
            format!("@rpath/{name}")
        } else {
            return err(format!("{name}: only .dylib files and .framework folders can be injected"));
        };
        if !already_loaded(&load) {
            m.add_dylib(&load)?;
        }
    }
    Ok(())
}

/// LC_RPATH already present? dyld refuses duplicate rpaths, so check first.
pub fn has_rpath(exe: &Path, path: &str) -> Result<bool> {
    let data = fs::read(exe)?;
    let found = match goblin::mach::Mach::parse(&data).map_err(|e| crate::Error::Message(e.to_string()))? {
        goblin::mach::Mach::Binary(m) => m.rpaths.contains(&path),
        goblin::mach::Mach::Fat(fat) => (0..fat.narches).any(|i| match fat.get(i) {
            Ok(goblin::mach::SingleArch::MachO(m)) => m.rpaths.contains(&path),
            _ => false,
        }),
    };
    Ok(found)
}

/// Load commands of a Mach-O (dylibs it links), for display and removal.
pub fn load_commands(path: &Path) -> Vec<String> {
    let Ok(data) = fs::read(path) else { return vec![] };
    let libs = |m: &goblin::mach::MachO| m.libs.iter().filter(|l| **l != "self").map(|s| s.to_string()).collect::<Vec<_>>();
    match goblin::mach::Mach::parse(&data) {
        Ok(goblin::mach::Mach::Binary(m)) => libs(&m),
        Ok(goblin::mach::Mach::Fat(fat)) => match fat.get(0) {
            Ok(goblin::mach::SingleArch::MachO(m)) => libs(&m),
            _ => vec![],
        },
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_suffix_is_added_once() {
        assert_eq!(installed_bundle_id("com.foo.app", "ABCDE12345"), "com.foo.app.ABCDE12345");
        assert_eq!(installed_bundle_id("com.foo.app.ABCDE12345", "ABCDE12345"), "com.foo.app.ABCDE12345");
    }

    #[test]
    fn prepare_rewrites_nested_ids_and_info() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("X.app");
        let ext = app.join("PlugIns/Share.appex");
        fs::create_dir_all(&ext).unwrap();
        let mut info = Dictionary::new();
        info.insert("CFBundleIdentifier".into(), "com.foo.app".into());
        info.insert("CFBundleExecutable".into(), "X".into());
        write_info(&app, &info).unwrap();
        let mut e = Dictionary::new();
        e.insert("CFBundleIdentifier".into(), "com.foo.app.share".into());
        write_info(&ext, &e).unwrap();

        let opts = InstallOptions { display_name: Some("Renamed".into()), enable_file_sharing: true, ..Default::default() };
        let (orig, new) = prepare(&app, &opts, "TEAM123456", &|_| false).unwrap();
        assert_eq!(orig, "com.foo.app");
        assert_eq!(new, "com.foo.app.TEAM123456");
        assert_eq!(string(&read_info(&ext).unwrap(), "CFBundleIdentifier"), "com.foo.app.TEAM123456.share");
        let i = read_info(&app).unwrap();
        assert_eq!(string(&i, "CFBundleDisplayName"), "Renamed");
        assert_eq!(i.get("UIFileSharingEnabled").and_then(Value::as_boolean), Some(true));

        let opts = InstallOptions { remove_extensions: true, ..Default::default() };
        prepare(&app, &opts, "TEAM123456", &|_| false).unwrap();
        assert!(!app.join("PlugIns").exists());
    }
}
