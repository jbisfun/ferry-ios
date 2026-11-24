# Ferry for iOS

On-device sideloading for iPhone and iPad, in SwiftUI. It's the iOS sibling of
[Ferry](../ferry) (Linux) and covers what SideStore does, without JIT and with a
more native interface:

- Sign in with any Apple ID (2FA, free or paid team); certificates, App IDs
  and device registration are handled for you.
- Install `.ipa` files from Files, Safari, AirDrop or a source; refresh them
  before they expire, with a reminder the day before.
- Sources: AltStore-format repositories (v1 and v2 layouts) and GitHub
  repositories, whose releases' `.ipa` assets become versions.
- Entitlements: see what every binary in an IPA asks for, which of those a
  free Apple ID can't get, what the source declares, and afterwards what the
  profile actually granted.
- Tweaks: inject `.dylib`s and `.framework`s, remove linked libraries, rename,
  change the bundle id, version or minimum iOS, drop extensions, enable Files
  sharing.

## How it works without a computer or JIT

A loopback VPN (LocalDevVPN or StosVPN) routes `10.7.0.1` to the phone's own
`lockdownd`. With the phone's pairing file, Ferry talks to it like a computer
would: AFC to upload, `installation_proxy` to install. Nothing runs in the
debugger, so no JIT is needed.

## Layout

```
core/      Rust static library: login, provisioning, signing, injection, install
App/       SwiftUI app (Core bridge, models, views)
idlproj.yml
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build (Linux)

Needs `xtool setup` (Darwin SDK), swiftly's Swift toolchain, `rustup target
add aarch64-apple-ios`, and [idl](../idl).

```sh
core/build-ios.sh            # core/out/libferry_core.a + ferry_core.h
idl build -c release         # build/release/Ferry.ipa (ad-hoc signed)
idl test -c release          # bundle checks
cargo test --manifest-path core/Cargo.toml
cargo run --manifest-path core/Cargo.toml --example smoke -- App.ipa [tweak.dylib]
```

The IPA must be re-signed to install (e.g. `idl install <udid> Ferry` through
Jimpactor, or desktop Ferry). After that Ferry refreshes itself like any other
app in its library once you add its IPA.

`core/Cargo.toml` patches `omnisette` to `../../omnisette` until the iOS
(remote-only) build gating there is pushed.
