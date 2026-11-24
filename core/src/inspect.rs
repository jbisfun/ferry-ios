//! Reads what an IPA is and what it asks for, without unpacking all of it:
//! Info.plists, the icon, and every Mach-O's entitlements and load commands.
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use plist::{Dictionary, Value};
use plume_core::MachO;
use zip::ZipArchive;

use crate::{
    Result, bundle, err,
    protocol::{BinaryInfo, Config, IpaInfo, plist_to_json},
};

fn read_entry(zip: &mut ZipArchive<fs::File>, name: &str) -> Option<Vec<u8>> {
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn plist_entry(zip: &mut ZipArchive<fs::File>, name: &str) -> Option<Dictionary> {
    plist::from_bytes(&read_entry(zip, name)?).ok()
}

/// Icon file names an Info.plist declares, best first.
fn icon_names(info: &Dictionary) -> Vec<String> {
    let mut names = vec![];
    for key in ["CFBundleIcons", "CFBundleIcons~ipad"] {
        if let Some(files) = info
            .get(key)
            .and_then(Value::as_dictionary)
            .and_then(|d| d.get("CFBundlePrimaryIcon"))
            .and_then(Value::as_dictionary)
            .and_then(|d| d.get("CFBundleIconFiles"))
            .and_then(Value::as_array)
        {
            names.extend(files.iter().rev().filter_map(Value::as_string).map(str::to_owned));
        }
    }
    if let Some(f) = info.get("CFBundleIconFile").and_then(Value::as_string) {
        names.push(f.to_owned());
    }
    names.push("AppIcon".into());
    names
}

pub fn inspect(cfg: &Config, path: &Path) -> Result<IpaInfo> {
    let size = fs::metadata(path)?.len();
    let mut zip = ZipArchive::new(fs::File::open(path)?)?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();

    // Payload/<X>.app/Info.plist
    let app_prefix = names
        .iter()
        .filter_map(|n| {
            let rest = n.strip_prefix("Payload/")?;
            let (app, tail) = rest.split_once('/')?;
            (app.ends_with(".app") && tail == "Info.plist").then(|| format!("Payload/{app}/"))
        })
        .next()
        .ok_or_else(|| crate::Error::Message("Not an iOS app: no Payload/*.app/Info.plist".into()))?;
    let info = plist_entry(&mut zip, &format!("{app_prefix}Info.plist")).ok_or_else(|| crate::Error::Message("Unreadable Info.plist".into()))?;
    let s = |k: &str| bundle::string(&info, k);
    let bundle_id = s("CFBundleIdentifier");
    if bundle_id.is_empty() {
        return err("The app has no bundle identifier");
    }

    let scratch = PathBuf::from(&cfg.tmp_dir).join(format!("inspect-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&scratch)?;

    // Largest PNG whose name starts with a declared icon name, at the app root.
    let mut icon_path = None;
    'icons: for want in icon_names(&info) {
        let mut best: Option<(u64, String)> = None;
        for n in &names {
            let Some(file) = n.strip_prefix(&app_prefix) else { continue };
            if file.contains('/') || !file.starts_with(&want) || !file.ends_with(".png") {
                continue;
            }
            let sz = zip.by_name(n).map(|f| f.size()).unwrap_or(0);
            if best.as_ref().is_none_or(|(b, _)| sz > *b) {
                best = Some((sz, n.clone()));
            }
        }
        if let Some((_, n)) = best {
            if let Some(bytes) = read_entry(&mut zip, &n) {
                let out = scratch.join("icon.png");
                fs::write(&out, bytes)?;
                icon_path = Some(out.to_string_lossy().into_owned());
                break 'icons;
            }
        }
    }

    // Executables: main, nested bundles', frameworks', loose dylibs.
    let mut exes: BTreeSet<(String, String)> = BTreeSet::new(); // (kind, path in app)
    exes.insert(("app".into(), s("CFBundleExecutable")));
    let mut extensions = vec![];
    for n in &names {
        let Some(rel) = n.strip_prefix(&app_prefix) else { continue };
        if rel.ends_with(".dylib") && !rel.contains(".framework/") {
            exes.insert(("dylib".into(), rel.to_owned()));
        }
        if let Some(dir) = rel.strip_suffix("/Info.plist") {
            let kind = if dir.ends_with(".appex") {
                "extension"
            } else if dir.ends_with(".framework") {
                "framework"
            } else if dir.ends_with(".app") && !dir.is_empty() {
                "app"
            } else {
                continue;
            };
            if let Some(d) = plist_entry(&mut zip, n) {
                let exe = bundle::string(&d, "CFBundleExecutable");
                if !exe.is_empty() {
                    exes.insert((kind.into(), format!("{dir}/{exe}")));
                }
                if kind == "extension" {
                    extensions.push(bundle::string(&d, "CFBundleIdentifier"));
                }
            }
        }
    }

    let mut binaries = vec![];
    for (kind, rel) in exes {
        let Some(bytes) = read_entry(&mut zip, &format!("{app_prefix}{rel}")) else { continue };
        let tmp = scratch.join("bin");
        fs::write(&tmp, &bytes)?;
        let entitlements = MachO::new(&tmp)
            .ok()
            .and_then(|m| m.entitlements().clone())
            .map(|d| plist_to_json(&Value::Dictionary(d)))
            .unwrap_or(serde_json::Value::Object(Default::default()));
        binaries.push(BinaryInfo { load_commands: bundle::load_commands(&tmp), path: rel, kind, entitlements });
        let _ = fs::remove_file(&tmp);
    }
    // The main executable first.
    binaries.sort_by_key(|b| (b.path != s("CFBundleExecutable"), b.path.clone()));

    Ok(IpaInfo {
        name: [s("CFBundleDisplayName"), s("CFBundleName")].into_iter().find(|n| !n.is_empty()).unwrap_or_else(|| bundle_id.clone()),
        version: s("CFBundleShortVersionString"),
        build: s("CFBundleVersion"),
        minimum_os: s("MinimumOSVersion"),
        bundle_id,
        icon_path,
        size,
        binaries,
        extensions,
    })
}
