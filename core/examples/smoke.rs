//! Host smoke test through the C ABI, the way the app uses it:
//!   cargo run --example smoke -- <app.ipa> [tweak.dylib]
//! Inspects the IPA, then unpacks it and runs the pre-signing edits
//! (re-identify + inject) and inspects the result again. No Apple ID or
//! device is involved.
use std::{
    ffi::{CStr, CString, c_char, c_void},
    sync::mpsc,
};

static mut TX: Option<mpsc::Sender<String>> = None;

extern "C" fn on_event(json: *const c_char, _: *mut c_void) {
    let s = unsafe { CStr::from_ptr(json) }.to_string_lossy().into_owned();
    #[allow(static_mut_refs)]
    if let Some(tx) = unsafe { TX.as_ref() } {
        let _ = tx.send(s);
    }
}

fn request(rx: &mpsc::Receiver<String>, json: serde_json::Value) -> serde_json::Value {
    let id = ferry_core::ferry_core_request(CString::new(json.to_string()).unwrap().as_ptr());
    loop {
        let ev: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        if ev["id"] != id {
            continue;
        }
        match ev["type"].as_str() {
            Some("result") => return ev["result"].clone(),
            Some("error") => panic!("{}", ev["message"]),
            Some("progress") => eprintln!("  .. {} {:?}", ev["stage"], ev["fraction"]),
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ipa = args.get(1).expect("usage: smoke <app.ipa> [tweak.dylib]");
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    unsafe { TX = Some(tx) };
    let config = serde_json::json!({ "data_dir": dir.path().join("data"), "tmp_dir": dir.path().join("tmp") });
    assert!(ferry_core::ferry_core_start(CString::new(config.to_string()).unwrap().as_ptr(), on_event, std::ptr::null_mut()));

    let methods = unsafe { CStr::from_ptr(ferry_core::ferry_core_methods()) }.to_string_lossy().into_owned();
    println!("methods: {methods}");

    let info = request(&rx, serde_json::json!({ "method": "ipa.inspect", "params": { "path": ipa } }));
    println!("inspect: {} {} ({}) min iOS {}", info["name"], info["version"], info["bundle_id"], info["minimum_os"]);
    println!("  icon: {}", info["icon_path"]);
    for b in info["binaries"].as_array().unwrap() {
        let ents = b["entitlements"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
        println!("  [{}] {}: {} entitlements {:?}; {} load commands", b["kind"], b["path"], ents.len(), ents, b["load_commands"].as_array().unwrap().len());
    }

    // Pre-signing edits on an unpacked copy.
    let stage = dir.path().join("stage");
    xkit::utilities::ZIPCompressor::decompress(std::path::Path::new(ipa), &stage, &|_| {}).unwrap();
    let app = ferry_core::bundle::find_app(&stage.join("Payload")).unwrap();
    let opts = ferry_core::protocol::InstallOptions {
        display_name: Some("Smoke".into()),
        inject: args.get(2).cloned().into_iter().collect(),
        ..Default::default()
    };
    let exe = app.join(ferry_core::bundle::string(&ferry_core::bundle::read_info(&app).unwrap(), "CFBundleExecutable"));
    let before = ferry_core::bundle::load_commands(&exe);
    let (orig, new) = ferry_core::bundle::prepare(&app, &opts, "ABCDE12345", &|l| before.iter().any(|b| b == l)).unwrap();
    println!("prepare: {orig} -> {new}");
    let after = ferry_core::bundle::load_commands(&exe);
    for lc in after.iter().filter(|l| !before.contains(l)) {
        println!("  added load command: {lc}");
    }
    println!("  rpath @executable_path/Frameworks: {}", ferry_core::bundle::has_rpath(&exe, "@executable_path/Frameworks").unwrap());
    if let Some(t) = args.get(2) {
        let name = std::path::Path::new(t).file_name().unwrap();
        println!("  copied into Frameworks/: {}", app.join("Frameworks").join(name).exists());
    }
    // Injecting twice must not add a second load command or rpath.
    let again = ferry_core::bundle::load_commands(&exe);
    ferry_core::bundle::prepare(&app, &opts, "ABCDE12345", &|l| again.iter().any(|b| b == l)).unwrap();
    assert_eq!(ferry_core::bundle::load_commands(&exe).len(), again.len(), "second injection duplicated a load command");
    println!("  re-running prepare is idempotent");

    let lib = request(&rx, serde_json::json!({ "method": "library.list" }));
    println!("library.list: {lib}");
    if std::env::var("FERRY_SMOKE_ANISETTE").is_ok() {
        // Provisions an anisette identity with the default remote server (no Apple ID involved).
        let cfg: ferry_core::protocol::Config = serde_json::from_value(config.clone()).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let data = rt.block_on(plume_core::auth::anisette_data::AnisetteData::new(ferry_core::account::anisette_config(&cfg)));
        match data {
            Ok(d) => println!("anisette from {}: {} headers ({})", cfg.anisette_url, d.base_headers.len(),
                d.base_headers.keys().filter(|k| k.starts_with("X-Apple-I-M")).cloned().collect::<Vec<_>>().join(", ")),
            Err(e) => println!("anisette from {} FAILED: {e}", cfg.anisette_url),
        }
    }
    let e = std::panic::catch_unwind(|| request(&rx, serde_json::json!({ "method": "device.connect" })));
    println!("device.connect without a pairing file fails cleanly: {}", e.is_err());
}
