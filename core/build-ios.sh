#!/usr/bin/env bash
# Cross-builds the Rust core for arm64 iOS on Linux and copies the static
# library + header where the app target links them (core/out/).
#   core/build-ios.sh [--debug]
set -euo pipefail
cd "$(dirname "$0")"
PROFILE=release
[ "${1:-}" = "--debug" ] && PROFILE=debug

SDK_BUNDLE="${SDK_BUNDLE:-$HOME/.swiftpm/swift-sdks/darwin.artifactbundle}"
IOS_SDK="${IOS_SDK:-$(ls -d "$SDK_BUNDLE"/Developer/Platforms/iPhoneOS.platform/Developer/SDKs/iPhoneOS*.sdk | sort -V | tail -1)}"
SWIFTLY_BIN="${SWIFTLY_BIN:-$HOME/.local/share/swiftly/bin}"
for f in "$IOS_SDK" "$SWIFTLY_BIN/clang"; do
    [ -e "$f" ] || { echo "missing $f (run 'xtool setup' / install swiftly)" >&2; exit 1; }
done
rustup target list --installed | grep -qx aarch64-apple-ios || { echo "run: rustup target add aarch64-apple-ios" >&2; exit 1; }

# C code in dependencies (ring, zstd, ...) is compiled with the Swift
# toolchain's clang against the iOS SDK.
export IPHONEOS_DEPLOYMENT_TARGET=16.0
export SDKROOT="$IOS_SDK"
export CC_aarch64_apple_ios="$SWIFTLY_BIN/clang"
export CXX_aarch64_apple_ios="$SWIFTLY_BIN/clang++"
export AR_aarch64_apple_ios="$SWIFTLY_BIN/llvm-ar"
export CFLAGS_aarch64_apple_ios="--target=arm64-apple-ios16.0 -isysroot $IOS_SDK"
export CXXFLAGS_aarch64_apple_ios="$CFLAGS_aarch64_apple_ios"

# On-device anisette runs Apple's Mac CoreADI under Unicorn. iOS forbids JIT,
# so Unicorn must be the interpreter-only (TCI) build, which
# ~/projs/WaffleStore/AnisetteKit/MacADI/build-ios.sh produces; unicorn-engine-sys
# links it through pkg-config instead of building its own (JIT) copy.
UNICORN_IOS_DIR="${UNICORN_IOS_DIR:-$HOME/projs/WaffleStore/AnisetteKit/MacADI/vendor/unicorn/build_ios_multi}"
[ -f "$UNICORN_IOS_DIR/libunicorn.a" ] && [ -f "$UNICORN_IOS_DIR/unicorn.pc" ] || {
    echo "missing TCI Unicorn in $UNICORN_IOS_DIR (run bash ~/projs/WaffleStore/AnisetteKit/MacADI/build-ios.sh, or set UNICORN_IOS_DIR)" >&2; exit 1; }
"$SWIFTLY_BIN/llvm-ar" t "$UNICORN_IOS_DIR/libunicorn.a" | grep -qx tci.c.o || { echo "$UNICORN_IOS_DIR/libunicorn.a is not a TCI build" >&2; exit 1; }
export PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_PATH="$UNICORN_IOS_DIR"
export LIBCLANG_PATH="${LIBCLANG_PATH:-$(dirname "$SWIFTLY_BIN")/toolchains/$(ls "$(dirname "$SWIFTLY_BIN")/toolchains" | sort -V | tail -1)/usr/lib}"
export BINDGEN_EXTRA_CLANG_ARGS="--target=arm64-apple-ios -isysroot $IOS_SDK"

# Machine code only (see Cargo.toml [profile.release]).
# Some dependencies (adi-core) also build as dylibs, which need a Mach-O linker.
IOS_LD="${IOS_LD:-$SDK_BUNDLE/toolset/bin/ld64.lld}"
SDK_VERSION="$(basename "$IOS_SDK" .sdk)"; SDK_VERSION="${SDK_VERSION#iPhoneOS}"
export CARGO_TARGET_AARCH64_APPLE_IOS_LINKER="$SWIFTLY_BIN/clang"
export CARGO_TARGET_AARCH64_APPLE_IOS_RUSTFLAGS="-C embed-bitcode=no -C link-arg=-fuse-ld=$IOS_LD -C link-arg=-isysroot -C link-arg=$IOS_SDK -C link-arg=-Wl,-platform_version,ios,16.0,$SDK_VERSION"

flags=(--target aarch64-apple-ios --lib)
[ "$PROFILE" = release ] && flags+=(--release)
cargo build "${flags[@]}"

mkdir -p out
cp "target/aarch64-apple-ios/$PROFILE/libferry_core.a" out/
cp include/ferry_core.h out/
echo "built core/out/libferry_core.a ($PROFILE)"
