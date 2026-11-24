//! Exercise the device link against a real iPhone (read-only):
//!   cargo run --example device_probe -- <pairing file> <iphone ip>
//! RPPairing files go through RemotePairing → CDTunnel → RSD; lockdown
//! files through lockdownd. Prints the device info and its user apps.
#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().collect();
    let (file, ip) = (args.get(1).expect("pairing file"), args.get(2).expect("iphone ip"));
    let dir = tempfile::tempdir().unwrap();
    let cfg = ferry_core::protocol::Config {
        data_dir: dir.path().to_string_lossy().into(),
        tmp_dir: dir.path().join("tmp").to_string_lossy().into(),
        anisette_mode: "local".into(),
        anisette_url: String::new(),
        device_address: ip.clone(),
        log_path: None,
    };
    std::fs::copy(file, ferry_core::device::pairing_path(&cfg)).unwrap();
    let kind = match ferry_core::device::pairing(&cfg).unwrap() {
        ferry_core::device::Pairing::Remote(_) => "RemotePairing",
        ferry_core::device::Pairing::Lockdown(_) => "lockdown",
    };
    println!("pairing: {kind}");
    let t = std::time::Instant::now();
    let mut link = match ferry_core::device::open(&cfg, None).await {
        Ok(l) => l,
        Err(e) => return println!("open failed: {e}"),
    };
    println!("connected in {:?}", t.elapsed());
    // device_probe <pairing> <ip> --put <bundle id> <file>: copy a file into the app's Documents.
    if let Some(i) = args.iter().position(|a| a == "--put") {
        let (bundle, src) = (&args[i + 1], &args[i + 2]);
        let name = std::path::Path::new(src).file_name().unwrap().to_string_lossy().into_owned();
        match link.put_document(bundle, &name, &std::fs::read(src).unwrap()).await {
            Ok(()) => println!("put Documents/{name} into {bundle}"),
            Err(e) => println!("put failed: {e}"),
        }
        return;
    }
    // device_probe <pairing> <ip> --get <bundle id> [file] [out]: list Documents / fetch a file.
    if let Some(i) = args.iter().position(|a| a == "--get") {
        let bundle = &args[i + 1];
        match link.documents(bundle, args.get(i + 2).map(String::as_str)).await {
            Ok((names, data)) => {
                println!("Documents: {names:?}");
                if let (Some(d), Some(out)) = (data, args.get(i + 3)) {
                    std::fs::write(out, &d).unwrap();
                    println!("saved {} bytes to {out}", d.len());
                }
            }
            Err(e) => println!("get failed: {e}"),
        }
        return;
    }
    match link.info().await {
        Ok(i) => println!("info: {} ({}) iOS {} udid {}", i.name, i.product_type, i.os_version, i.udid),
        Err(e) => println!("info failed: {e}"),
    }
    match link.apps().await {
        Ok(a) => {
            println!("{} user apps; sideloaded:", a.len());
            for app in a.iter().filter(|a| a.sideloaded) {
                println!("  {} {} ({})", app.name, app.version, app.bundle_id);
            }
        }
        Err(e) => println!("apps failed: {e}"),
    }
}
