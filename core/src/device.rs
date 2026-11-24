//! Talking to this very iPhone. A loopback VPN (LocalDevVPN, StosVPN) routes
//! `device_address` back to the phone's own lockdownd, and the imported
//! pairing file authenticates us — no computer and no JIT involved.
use std::{net::IpAddr, path::PathBuf, sync::Arc};

use idevice::{
    IdeviceError, IdeviceService, installation_proxy::InstallationProxyClient, lockdown::LockdownClient,
    pairing_file::PairingFile, provider::TcpProvider,
};
use xkit::installation::Connection;

use crate::{
    Error, Result, err,
    protocol::{Config, DeviceInfo, InstalledApp},
};

pub fn pairing_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.data_dir).join("pairing.plist")
}

pub fn provider(cfg: &Config) -> Result<TcpProvider> {
    let path = pairing_path(cfg);
    if !path.exists() {
        return err("No pairing file yet. Import one in Settings → Device.");
    }
    let pairing_file = PairingFile::read_from_file(&path)?;
    let addr: IpAddr = cfg
        .device_address
        .parse()
        .map_err(|_| Error::Message(format!("'{}' is not an IP address", cfg.device_address)))?;
    Ok(TcpProvider { addr, scope_id: None, pairing_file, label: "Ferry".into() })
}

/// Human messages for the failures people actually hit.
pub fn describe(e: &IdeviceError) -> String {
    let s = e.to_string();
    let lower = s.to_lowercase();
    if lower.contains("connection refused") || lower.contains("timed out") || lower.contains("unreachable") || lower.contains("no route") {
        "Can't reach this device. Turn on the loopback VPN (LocalDevVPN or StosVPN) and try again.".into()
    } else if lower.contains("invalidhostid") || lower.contains("invalid host") || lower.contains("pairing") {
        "The pairing file was rejected. Make a new one for this iPhone (e.g. with Jimpactor or idevice_pair) and import it.".into()
    } else {
        s
    }
}

async fn lockdown(cfg: &Config) -> Result<(TcpProvider, LockdownClient)> {
    let p = provider(cfg)?;
    let mut client = LockdownClient::connect(&p).await?;
    client.start_session(&p.pairing_file).await?;
    Ok((p, client))
}

pub async fn udid(cfg: &Config) -> Result<String> {
    let (p, mut lockdown) = lockdown(cfg).await?;
    if let Some(u) = p.pairing_file.udid.clone() {
        return Ok(u);
    }
    Ok(lockdown.get_value(Some("UniqueDeviceID"), None).await?.as_string().unwrap_or_default().to_owned())
}

pub async fn info(cfg: &Config) -> Result<DeviceInfo> {
    let (p, mut l) = lockdown(cfg).await?;
    let mut get = async |k: &str| -> String {
        l.get_value(Some(k), None).await.ok().and_then(|v| v.as_string().map(str::to_owned)).unwrap_or_default()
    };
    let udid = match p.pairing_file.udid.clone() {
        Some(u) => u,
        None => get("UniqueDeviceID").await,
    };
    Ok(DeviceInfo {
        udid,
        name: get("DeviceName").await,
        product_type: get("ProductType").await,
        os_version: get("ProductVersion").await,
    })
}

/// An xkit connection for AFC uploads and installation_proxy.
pub async fn connection(cfg: &Config) -> Result<Arc<Connection>> {
    let p = provider(cfg)?;
    let udid = match p.pairing_file.udid.clone() {
        Some(u) => u,
        None => udid(cfg).await?,
    };
    Ok(Connection::with_provider(udid, Arc::new(p)).await?)
}

pub async fn apps(cfg: &Config) -> Result<Vec<InstalledApp>> {
    let p = provider(cfg)?;
    let mut proxy = InstallationProxyClient::connect(&p).await?;
    let apps = proxy.get_apps(Some("User"), None).await?;
    let mut out: Vec<InstalledApp> = apps
        .into_iter()
        .filter_map(|(bundle_id, v)| {
            let d = v.as_dictionary()?;
            let s = |k: &str| d.get(k).and_then(|v| v.as_string()).unwrap_or_default().to_owned();
            let name = [s("CFBundleDisplayName"), s("CFBundleName")].into_iter().find(|n| !n.is_empty()).unwrap_or_else(|| bundle_id.clone());
            let signer = s("SignerIdentity");
            Some(InstalledApp {
                sideloaded: signer.starts_with("Apple Development") || signer.starts_with("iPhone Developer"),
                bundle_id,
                name,
                version: s("CFBundleShortVersionString"),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

pub async fn uninstall(cfg: &Config, bundle_id: &str) -> Result<()> {
    let p = provider(cfg)?;
    let mut proxy = InstallationProxyClient::connect(&p).await?;
    proxy.uninstall(bundle_id, None).await?;
    Ok(())
}
