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

# Machine code only (see Cargo.toml [profile.release]).
export CARGO_TARGET_AARCH64_APPLE_IOS_RUSTFLAGS="-C embed-bitcode=no"

flags=(--target aarch64-apple-ios --lib)
[ "$PROFILE" = release ] && flags+=(--release)
cargo build "${flags[@]}"

mkdir -p out
cp "target/aarch64-apple-ios/$PROFILE/libferry_core.a" out/
cp include/ferry_core.h out/
echo "built core/out/libferry_core.a ($PROFILE)"
