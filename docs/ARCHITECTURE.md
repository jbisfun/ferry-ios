# Architecture

```
SwiftUI views ──> AppModel (@Observable, main actor)
                     │  SourceStore, Downloads, Keychain    (Swift only)
                     ▼
                  Core.swift ── C ABI (JSON) ──> core/ (Rust, libferry_core.a)
                                                 ├ account   plume_core GrandSlam + 2FA, omnisette anisette v3
                                                 ├ device    idevice over TcpProvider(10.7.0.1, pairing file)
                                                 ├ inspect   zip + goblin + apple-codesign entitlements
                                                 ├ bundle    Info.plist edits, re-identify, inject (plume_core MachO)
                                                 ├ install   xkit AutoSigner (provision + sign) → AppInstaller
                                                 └ library   kept IPAs + options, refresh
```

## Rules

- The core owns everything that talks to Apple or the device. Swift never
  touches lockdown, certificates or Mach-O files; it sends requests.
- The protocol is declared once in `core/src/protocol.rs`. `App/Sources/Core/Protocol.swift`
  mirrors it, and debug builds assert at launch that both list the same
  methods (`ferry_core_methods`).
- One callback carries every event: `result`, `error`, `progress` (tagged with
  the request id), `prompt` (2FA code, certificate revocation, answered with
  `ferry_core_respond`) and `log`.
- Cancelling the Swift task cancels the core request.
- Sources, downloads and the Keychain are app concerns and stay in Swift;
  they don't need the core.

## Lifetimes

- Apple sessions can't be persisted, so the Apple ID lives in the Keychain and
  the core signs in again at launch. The anisette identity is kept in
  `Application Support/Ferry/anisette`, so Apple keeps trusting the device and
  skips 2FA.
- `Application Support/Ferry/library/<original id>/` keeps the original IPA,
  icon and `record.json` (install options, expiry, granted entitlements);
  a refresh is a re-install from that with the same options.
- Injected tweaks are copied to `Application Support/Ferry/tweaks/` so
  refreshes can inject them again.
- Installs and refreshes are serialised by the core (`device_lock`).

## Identifiers

Free App IDs must be unique per team, so apps install as `<id>.<TEAMID>`
(the AltStore/SideStore convention). Nested extensions keep their suffix under
the new prefix. The original id is kept in `FerryOriginalBundleIdentifier`.

## Not done yet

- `.deb` tweaks (only `.dylib`/`.framework`); bundling ElleKit for
  Substrate tweaks automatically.
- Background refresh (BGTaskScheduler can't bring the VPN up).
- App data backup/restore, JIT enabling for other apps, pairing-file creation
  on device.
