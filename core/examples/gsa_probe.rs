//! Does Apple's GSA accept our anisette? Runs the SRP init with a made-up
//! Apple ID (no real account is touched):
//!   cargo run --example gsa_probe -- [local|remote] [anisette_url]
//! Accepted anisette -> an account error (e.g. -20209 / unknown user);
//! rejected anisette -> HTTP 503 from gsa.apple.com.
use plume_core::auth::Account;

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = env_logger_lite();
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).cloned().unwrap_or_else(|| "local".into());
    let dir = std::env::var("FERRY_PROBE_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| tempfile::tempdir().unwrap().keep());
    let cfg = ferry_core::protocol::Config {
        data_dir: dir.to_string_lossy().into(),
        tmp_dir: dir.join("tmp").to_string_lossy().into(),
        anisette_mode: mode.clone(),
        anisette_url: args.get(2).cloned().unwrap_or_else(ferry_core::protocol::default_anisette),
        device_address: String::new(),
    };
    let email = std::env::var("FERRY_PROBE_EMAIL").unwrap_or_else(|_| format!("ferry-probe-{}@example.com", uuid::Uuid::new_v4().simple()));
    if std::env::var("FERRY_PROBE_RAW").is_ok() {
        // Hand-built SRP init: shows Apple's status and body.
        let a = plume_core::auth::anisette_data::AnisetteData::new(ferry_core::account::anisette_config(&cfg)).await.unwrap();
        let mut cpd = a.to_plist(true, false, false);
        if std::env::var("FERRY_PROBE_SIDESIGN").is_ok() {
            for (k, v) in [("bootstrap", plist::Value::Boolean(true)), ("icscrec", true.into()), ("pbe", false.into()), ("prkgen", true.into()),
                           ("svct", "iCloud".into()), ("loc", "en_US".into()), ("X-Apple-Locale", "en_US".into())] {
                cpd.insert(k.into(), v);
            }
        }
        if std::env::var("FERRY_PROBE_EMPTY_CPD").is_ok() { cpd = plist::Dictionary::new(); }
        eprintln!("cpd: {:?}", cpd.iter().map(|(k, v)| format!("{k}={}", match v { plist::Value::String(s) if s.len() > 40 => format!("<{} chars>", s.len()), o => format!("{o:?}") })).collect::<Vec<_>>());
        eprintln!("client-info: {:?}", a.get_header("x-mme-client-info"));
        let body = plist::Value::Dictionary(plist::Dictionary::from_iter([
            ("Header".to_string(), plist::Value::Dictionary(plist::Dictionary::from_iter([("Version".to_string(), plist::Value::String("1.0.1".into()))]))),
            ("Request".to_string(), plist::Value::Dictionary(plist::Dictionary::from_iter([
                ("A2k".to_string(), plist::Value::Data({
                    let client = srp::Client::<srp::groups::G2048, sha2::Sha256>::new_with_options(false);
                    client.compute_public_ephemeral(&rand_a())
                })),
                ("cpd".to_string(), plist::Value::Dictionary(cpd)),
                ("o".to_string(), plist::Value::String("init".into())),
                ("ps".to_string(), plist::Value::Array(vec!["s2k".into(), "s2k_fo".into()])),
                ("u".to_string(), plist::Value::String(email.clone())),
            ]))),
        ]));
        let mut buf = vec![];
        plist::to_writer_xml(&mut buf, &body).unwrap();
        let r = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap() // probe only: GSA uses Apple's own root
            .post("https://gsa.apple.com/grandslam/GsService2")
            .header("Content-Type", "text/x-xml-plist")
            .header("Accept", "*/*")
            .header("User-Agent", std::env::var("FERRY_PROBE_UA").unwrap_or_else(|_| a.user_agent().to_string()))
            .header("X-MMe-Client-Info", a.get_header("x-mme-client-info").unwrap())
            .body(buf)
            .send()
            .await
            .unwrap();
        println!("{mode} UA={:?}: HTTP {}", std::env::var("FERRY_PROBE_UA").ok(), r.status());
        let text = r.text().await.unwrap();
        println!("{}", &text[..text.len().min(1500)]);
        return;
    }
    let r = Account::login(
        move || Ok((email.clone(), "not-a-password".into())),
        |_| Err("no 2FA in probe".into()),
        ferry_core::account::anisette_config(&cfg),
    )
    .await;
    match r {
        Ok(_) => println!("{mode}: login unexpectedly succeeded"),
        Err(e) => println!("{mode}: {e}"),
    }
}

fn rand_a() -> Vec<u8> {
    (0..32).map(|_| uuid::Uuid::new_v4().as_bytes()[0]).collect()
}

fn env_logger_lite() -> Result<(), log::SetLoggerError> {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Debug && !m.target().starts_with("hyper") && !m.target().starts_with("rustls")
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{}] {}: {}", r.level(), r.target(), r.args());
            }
        }
        fn flush(&self) {}
    }
    log::set_boxed_logger(Box::new(L)).map(|()| log::set_max_level(log::LevelFilter::Debug))
}
