//! Talking to this very iPhone. A loopback VPN (LocalDevVPN, StosVPN) routes
//! the tunnel's peer address back to the phone itself; the imported pairing
//! file authenticates us. No computer and no JIT involved.
//!
//! Two pairing types, two paths (as SideStore's minimuxer does):
//! - RPPairing (`rppairing.plist`: public_key/private_key/identifier):
//!   RemotePairing on port 49152 → TLS-PSK CDTunnel → software TCP stack →
//!   RSD → `.shim.remote` services. Works over the plain VPN on iOS 17+,
//!   and is the only option on iOS 26.4+ unless the VPN is IPsec.
//! - Lockdown (`pairing.plist`: HostCertificate/HostPrivateKey): lockdownd on
//!   port 62078. Older iOS, or an IPsec/IKEv2 VPN on iOS 26.4+.
use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use idevice::{
    IdeviceError, IdeviceService, RsdService,
    afc::{AfcClient, opcode::AfcFopenMode},
    installation_proxy::InstallationProxyClient,
    lockdown::LockdownClient,
    pairing_file::PairingFile,
    provider::TcpProvider,
    remote_pairing::{RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native},
    rsd::RsdHandshake,
    tcp::{adapter::Adapter, handle::AdapterHandle},
};
use tokio::{io::AsyncReadExt, net::TcpStream};
use xkit::installation::{AppInstaller, Connection, ConnectionPreferences, InstallationStage};

use crate::{
    Error, Result, err,
    protocol::{Config, DeviceInfo, InstalledApp},
};

const LOCKDOWN_PORT: u16 = 62078;
const REMOTE_PAIRING_PORT: u16 = 49152;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

pub fn pairing_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.data_dir).join("pairing.plist")
}

pub fn rp_pairing_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.data_dir).join("rppairing.plist")
}

/// Human messages for the failures people actually hit.
pub fn describe(e: &IdeviceError) -> String {
    let s = e.to_string();
    let lower = s.to_lowercase();
    if lower.contains("connection refused") || lower.contains("timed out") || lower.contains("unreachable") || lower.contains("no route") || lower.contains("socket") {
        format!("Can't reach this iPhone through the VPN ({s}). Turn on LocalDevVPN or StosVPN and try again.")
    } else if lower.contains("invalidhostid") || lower.contains("invalid host") || lower.contains("pairing") {
        format!("The pairing file was rejected ({s}). Make a new one for this iPhone and import it.")
    } else {
        s
    }
}

// ---- address ----------------------------------------------------------------

/// The VPN tunnel's peer (the phone as seen through the loopback VPN): the
/// destination address of a point-to-point utun/ipsec interface.
#[cfg(any(target_os = "ios", target_os = "macos"))]
fn tunnel_peer() -> Option<IpAddr> {
    use std::ffi::CStr;
    let mut ifaddrs: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifaddrs) } != 0 {
        return None;
    }
    let mut found = None;
    let mut cur = ifaddrs;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy();
        if !(name.starts_with("utun") || name.starts_with("ipsec")) || ifa.ifa_flags & libc::IFF_POINTOPOINT as u32 == 0 {
            continue;
        }
        let dst = ifa.ifa_dstaddr;
        if dst.is_null() || unsafe { (*dst).sa_family } as i32 != libc::AF_INET {
            continue;
        }
        let sin = unsafe { &*(dst as *const libc::sockaddr_in) };
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        // System tunnels (iCloud Private Relay etc.) don't use private peers.
        if ip.is_private() {
            found = Some(IpAddr::V4(ip));
            if name.starts_with("utun") {
                break;
            }
        }
    }
    unsafe { libc::freeifaddrs(ifaddrs) };
    found
}

#[cfg(not(any(target_os = "ios", target_os = "macos")))]
fn tunnel_peer() -> Option<IpAddr> {
    None
}

pub fn address(cfg: &Config) -> Result<IpAddr> {
    let a = cfg.device_address.trim();
    if a.is_empty() || a.eq_ignore_ascii_case("auto") {
        return Ok(tunnel_peer().unwrap_or(IpAddr::V4(Ipv4Addr::new(10, 7, 0, 1))));
    }
    a.parse().map_err(|_| Error::Message(format!("'{a}' is not an IP address")))
}

async fn tcp(addr: IpAddr, port: u16) -> Result<TcpStream> {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((addr, port))).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => err(format!(
            "Can't reach this iPhone at {addr}:{port} ({e}). Turn on LocalDevVPN or StosVPN and try again."
        )),
        Err(_) => err(format!("Timed out reaching {addr}:{port}. Is the loopback VPN on?")),
    }
}

// ---- links --------------------------------------------------------------------

/// An RSD session over the RemotePairing tunnel. Dropping it closes the tunnel.
pub struct RsdLink {
    adapter: AdapterHandle,
    handshake: RsdHandshake,
}

impl RsdLink {
    /// Open an RSD service by port; the futures stay `Send`, unlike `connect_rsd`.
    async fn service<T: RsdService>(&mut self) -> Result<T> {
        let name = T::rsd_service_name();
        let port = self
            .handshake
            .services
            .get(name.as_ref())
            .map(|s| s.port)
            .ok_or_else(|| Error::Message(format!("This iPhone doesn't offer {name}")))?;
        let stream = self.adapter.connect(port).await?;
        Ok(T::from_stream(Box::new(stream)).await?)
    }
}

pub enum Link {
    Rsd(RsdLink),
    Lockdown { conn: Arc<Connection>, provider: TcpProvider },
}

/// Which pairing is configured. RPPairing wins when both exist.
pub enum Pairing {
    Remote(RpPairingFile),
    Lockdown(PairingFile),
}

pub fn pairing(cfg: &Config) -> Result<Pairing> {
    if let Ok(bytes) = std::fs::read(rp_pairing_path(cfg)) {
        return Ok(Pairing::Remote(RpPairingFile::from_bytes(&bytes)?));
    }
    if let Ok(bytes) = std::fs::read(pairing_path(cfg)) {
        // A RemotePairing file imported under the lockdown name still works.
        if let Ok(rp) = RpPairingFile::from_bytes(&bytes) {
            return Ok(Pairing::Remote(rp));
        }
        return Ok(Pairing::Lockdown(PairingFile::from_bytes(&bytes)?));
    }
    err("No pairing file yet. Import one in Settings → This iPhone.")
}

/// Asked for the code the iPhone shows if it wants to pair again.
pub type PinPrompt<'a> = &'a (dyn Fn() -> futures::future::BoxFuture<'static, String> + Send + Sync);

pub async fn open(cfg: &Config, pin: Option<PinPrompt<'_>>) -> Result<Link> {
    let addr = address(cfg)?;
    match pairing(cfg)? {
        Pairing::Remote(mut rp) => {
            let stream = tcp(addr, REMOTE_PAIRING_PORT).await?;
            let mut client = RemotePairingClient::new(RpPairingSocket::new(stream), "Ferry");
            let before = rp.to_bytes();
            let no_pin = || -> futures::future::BoxFuture<'static, String> { Box::pin(async { String::new() }) };
            let pin: PinPrompt = pin.unwrap_or(&no_pin);
            client.connect(&mut rp, || pin()).await.map_err(|e| {
                Error::Message(format!("This iPhone didn't accept the RemotePairing file ({}). Make a new one for it and import it.", describe(&e)))
            })?;
            if rp.to_bytes() != before {
                // Re-paired: keep the new keys.
                let _ = std::fs::write(rp_pairing_path(cfg), rp.to_bytes());
            }
            let port = client.create_tcp_listener().await?;
            let tunnel = connect_tls_psk_tunnel_native(tcp(addr, port).await?, client.encryption_key()).await?;
            let info = tunnel.info.clone();
            let ours: IpAddr = info.client_address.parse().map_err(|_| Error::Message("Bad tunnel address".into()))?;
            let theirs: IpAddr = info.server_address.parse().map_err(|_| Error::Message("Bad tunnel address".into()))?;
            let mut adapter = Adapter::new(Box::new(tunnel.into_inner()), ours, theirs);
            adapter.set_mss((info.mtu as usize).saturating_sub(60));
            let mut adapter = adapter.to_async_handle();
            let handshake = RsdHandshake::new(adapter.connect(info.server_rsd_port).await?).await?;
            Ok(Link::Rsd(RsdLink { adapter, handshake }))
        }
        Pairing::Lockdown(pairing_file) => {
            // Fail fast with a clear message if nothing listens.
            drop(tcp(addr, LOCKDOWN_PORT).await.map_err(|e| {
                Error::Message(format!("{e} On iOS 26.4 and later a plain VPN can't reach lockdownd: import a RemotePairing (RPPairing) file instead, or use an IPsec VPN."))
            })?);
            let provider = TcpProvider { addr, scope_id: None, pairing_file: pairing_file.clone(), label: "Ferry".into() };
            let udid = match pairing_file.udid.clone() {
                Some(u) => u,
                None => {
                    let mut l = LockdownClient::connect(&provider).await?;
                    l.start_session(&pairing_file).await?;
                    l.get_value(Some("UniqueDeviceID"), None).await?.as_string().unwrap_or_default().to_owned()
                }
            };
            let conn_provider = TcpProvider { addr, scope_id: None, pairing_file, label: "Ferry".into() };
            Ok(Link::Lockdown { conn: Connection::with_provider(udid, Arc::new(conn_provider)).await?, provider })
        }
    }
}

impl Link {
    async fn lockdown(&mut self) -> Result<LockdownClient> {
        match self {
            Link::Rsd(r) => r.service::<LockdownClient>().await,
            Link::Lockdown { provider, .. } => {
                let mut l = LockdownClient::connect(&*provider).await?;
                l.start_session(&provider.pairing_file).await?;
                Ok(l)
            }
        }
    }

    pub async fn info(&mut self) -> Result<DeviceInfo> {
        let mut l = self.lockdown().await?;
        let mut get = async |k: &str| -> String {
            l.get_value(Some(k), None).await.ok().and_then(|v| v.as_string().map(str::to_owned)).unwrap_or_default()
        };
        Ok(DeviceInfo {
            udid: get("UniqueDeviceID").await,
            name: get("DeviceName").await,
            product_type: get("ProductType").await,
            os_version: get("ProductVersion").await,
        })
    }

    async fn installer(&mut self) -> Result<InstallationProxyClient> {
        match self {
            Link::Rsd(r) => r.service::<InstallationProxyClient>().await,
            Link::Lockdown { provider, .. } => InstallationProxyClient::connect(&*provider).await.map_err(lockdown_service_error),
        }
    }

    pub async fn apps(&mut self) -> Result<Vec<InstalledApp>> {
        let apps = self.installer().await?.get_apps(Some("User"), None).await?;
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

    /// Write a file into an app's Documents (house_arrest; the app needs
    /// UIFileSharingEnabled). Used by tooling to hand Ferry its pairing file.
    pub async fn put_document(&mut self, bundle_id: &str, name: &str, bytes: &[u8]) -> Result<()> {
        let ha: idevice::house_arrest::HouseArrestClient = match self {
            Link::Rsd(r) => r.service().await?,
            Link::Lockdown { provider, .. } => idevice::house_arrest::HouseArrestClient::connect(&*provider).await?,
        };
        let mut afc = ha.vend_documents(bundle_id).await?;
        let mut fd = afc.open(format!("/Documents/{name}"), AfcFopenMode::WrOnly).await?;
        let written = fd.write_entire(bytes).await;
        let closed = fd.close().await;
        written?;
        closed?;
        Ok(())
    }

    pub async fn uninstall(&mut self, bundle_id: &str) -> Result<()> {
        self.installer().await?.uninstall(bundle_id, None).await?;
        Ok(())
    }

    /// Upload a signed IPA and install it. `progress(stage, fraction)`.
    pub async fn install(&mut self, ipa: &std::path::Path, bundle_id: &str, progress: &(dyn Fn(&str, Option<f64>) + Send + Sync)) -> Result<()> {
        match self {
            Link::Lockdown { conn, .. } => {
                AppInstaller { ipa: ipa.to_owned(), udid: conn.udid.clone(), connection_preferences: ConnectionPreferences::default() }
                    .install_with_connection(conn, &|st: InstallationStage| progress(st.display_name(), Some(st.display_progress())))
                    .await
                    .map_err(|e| match e {
                        xkit::Error::Device(d) => lockdown_service_error(d),
                        other => other.into(),
                    })?;
                Ok(())
            }
            Link::Rsd(r) => {
                // Start installation_proxy first, as xkit does, so it doesn't time out during the upload.
                let mut inst = r.service::<InstallationProxyClient>().await?;
                let mut afc = r.service::<AfcClient>().await?;
                let _ = afc.mk_dir("PublicStaging").await;
                let remote = format!("PublicStaging/ferry-{}.ipa", uuid::Uuid::new_v4().simple());
                let total = std::fs::metadata(ipa)?.len().max(1) as f64;
                let mut file = tokio::fs::File::open(ipa).await?;
                let mut fd = afc.open(remote.clone(), AfcFopenMode::WrOnly).await?;
                let mut buf = vec![0u8; 1 << 20];
                let mut sent = 0u64;
                let upload: Result<()> = async {
                    loop {
                        let n = file.read(&mut buf).await?;
                        if n == 0 {
                            break;
                        }
                        fd.write_entire(&buf[..n]).await?;
                        sent += n as u64;
                        progress("Uploading", Some(sent as f64 / total));
                    }
                    Ok(())
                }
                .await;
                let closed = fd.close().await;
                upload?;
                closed?;
                let options = plist::Value::Dictionary(plist::Dictionary::from_iter([(
                    "CFBundleIdentifier".to_string(),
                    plist::Value::String(bundle_id.to_owned()),
                )]));
                let result = inst
                    .install_with_callback(
                        remote.clone(),
                        Some(options),
                        |(pct, _): (u64, ())| async move { progress("Installing", Some(pct as f64 / 100.0)) },
                        (),
                    )
                    .await;
                let _ = afc.remove(remote).await;
                result?;
                Ok(())
            }
        }
    }
}

/// iOS 26.4+ answers lockdown queries over a plain VPN but drops the
/// connection when a service is started ("socket io failed").
fn lockdown_service_error(e: IdeviceError) -> Error {
    let s = e.to_string();
    if s.to_lowercase().contains("socket") {
        Error::Message(format!(
            "This iPhone refuses lockdown services over the VPN ({s}); iOS 26.4 and later need a RemotePairing (RPPairing) file. Import one in Settings → This iPhone."
        ))
    } else {
        e.into()
    }
}

// ---- one-shot helpers for the protocol ----------------------------------------

pub async fn info(cfg: &Config) -> Result<DeviceInfo> {
    open(cfg, None).await?.info().await
}

pub async fn apps(cfg: &Config) -> Result<Vec<InstalledApp>> {
    open(cfg, None).await?.apps().await
}

pub async fn uninstall(cfg: &Config, bundle_id: &str) -> Result<()> {
    open(cfg, None).await?.uninstall(bundle_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(addr: &str) -> Config {
        Config {
            data_dir: "/nonexistent".into(),
            tmp_dir: "/tmp".into(),
            anisette_mode: "local".into(),
            anisette_url: String::new(),
            device_address: addr.into(),
        }
    }

    #[test]
    fn address_auto_and_explicit() {
        assert_eq!(address(&cfg("auto")).unwrap(), IpAddr::V4(Ipv4Addr::new(10, 7, 0, 1)));
        assert_eq!(address(&cfg("")).unwrap(), IpAddr::V4(Ipv4Addr::new(10, 7, 0, 1)));
        assert_eq!(address(&cfg("10.8.0.1")).unwrap(), IpAddr::V4(Ipv4Addr::new(10, 8, 0, 1)));
        assert!(address(&cfg("nope")).is_err());
    }

    #[test]
    fn pairing_prefers_remote_and_detects_type() {
        let d = tempfile::tempdir().unwrap();
        let mut c = cfg("auto");
        c.data_dir = d.path().to_string_lossy().into();
        assert!(pairing(&c).is_err());
        // An RP file saved under the lockdown name is still recognised.
        std::fs::write(pairing_path(&c), RpPairingFile::generate("test").to_bytes()).unwrap();
        assert!(matches!(pairing(&c).unwrap(), Pairing::Remote(_)));
    }
}
