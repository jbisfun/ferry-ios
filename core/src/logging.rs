//! Core log: every `log` record (and idevice's `tracing` events, bridged by
//! tracing's `log` feature) goes to a file the user can share (Ferry's
//! Documents, so it can also be pulled with house_arrest) and, at info and
//! above, to the app's Log screen.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::{Arc, Mutex},
};

use crate::Shared;

const MAX_BYTES: u64 = 4 << 20;

struct Logger {
    file: Mutex<Option<File>>,
    shared: Arc<Shared>,
}

fn wanted(target: &str, level: log::Level) -> bool {
    let ours = ["ferry_core", "plume_core", "omnisette", "xkit", "idevice", "jktcp"];
    if ours.iter().any(|t| target.starts_with(t)) {
        return level <= log::Level::Debug;
    }
    level <= log::Level::Info && !target.starts_with("rustls") && !target.starts_with("hyper")
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        wanted(m.target(), m.level())
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!("{} {:<5} {}: {}", chrono::Utc::now().format("%H:%M:%S%.3f"), r.level(), r.target(), r.args());
        if let Ok(mut f) = self.file.lock() {
            if let Some(file) = f.as_mut() {
                let _ = writeln!(file, "{line}");
            }
        }
        if r.level() <= log::Level::Info {
            self.shared.log(&r.level().to_string().to_lowercase(), format!("{}: {}", r.target(), r.args()));
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            if let Some(file) = f.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

pub fn init(path: Option<&str>, shared: Arc<Shared>) {
    let file = path.and_then(|p| {
        // Start over when it gets big; the last session is what matters.
        if std::fs::metadata(p).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
            let _ = std::fs::remove_file(p);
        }
        OpenOptions::new().create(true).append(true).open(p).ok()
    });
    if log::set_boxed_logger(Box::new(Logger { file: Mutex::new(file), shared })).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
    log::info!("Ferry core {} started", env!("CARGO_PKG_VERSION"));
}
