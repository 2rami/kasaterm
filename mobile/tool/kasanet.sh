#!/usr/bin/env bash
# 카사넷 C ABI(crates/kasa-net-ffi)를 iOS 정적 라이브러리로 굽는다 → ios/KasaNet/KasaNet.xcframework.
# 기기(aarch64-apple-ios)·시뮬레이터(aarch64-apple-ios-sim + x86_64-apple-ios 합본) 두 조각. 시뮬레이터 빌드는
# x86_64 도 요구해 arm64 만 든 조각은 고르지 않는다. 결과물은 커밋하지 않는다 — flutter build ios
# 앞에 이것을 먼저 부른다(sim.sh·testflight.sh·phone.sh 가 부른다). 바뀐 게 없으면 cargo 가 곧바로 끝낸다.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(cd .. && pwd)
out=ios/KasaNet/KasaNet.xcframework
targets=(aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios)
rustup target add "${targets[@]}" >/dev/null 2>&1 || true
export IPHONEOS_DEPLOYMENT_TARGET=15.0
for t in "${targets[@]}"; do
  cargo build --manifest-path "$root/Cargo.toml" -p kasa-net-ffi --release --lib --target "$t" \
    --target-dir "$root/target/ios" 2>&1 | grep -E "^(error|warning: unused)" || true
  [ -f "$root/target/ios/$t/release/libkasa_net_ffi.a" ] || { echo "kasa-net-ffi($t) 굽기 실패" >&2; exit 1; }
done
sim="$root/target/ios/sim/libkasa_net_ffi.a"
mkdir -p "$(dirname "$sim")"
lipo -create "$root/target/ios/aarch64-apple-ios-sim/release/libkasa_net_ffi.a" \
  "$root/target/ios/x86_64-apple-ios/release/libkasa_net_ffi.a" -output "$sim"
rm -rf "$out"
xcodebuild -create-xcframework \
  -library "$root/target/ios/aarch64-apple-ios/release/libkasa_net_ffi.a" \
  -library "$sim" \
  -output "$out" >/dev/null
echo "카사넷 → $out"
