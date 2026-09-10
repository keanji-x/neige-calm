#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../p2p-native"
: "${NDK_HOME:?Set NDK_HOME to Android NDK29}"
abi=${1:-arm64-v8a}
case "$abi" in
  arm64-v8a) goarch=arm64; compiler=aarch64-linux-android26 ;;
  armeabi-v7a) goarch=arm; compiler=armv7a-linux-androideabi26 ;;
  x86) goarch=386; compiler=i686-linux-android26 ;;
  x86_64) goarch=amd64; compiler=x86_64-linux-android26 ;;
  *) echo "Unsupported Android ABI: $abi" >&2; exit 1 ;;
esac
export GOOS=android GOARCH="$goarch" CGO_ENABLED=1
export CC="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/$compiler-clang"
export CXX="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/$compiler-clang++"
output="../src-tauri/gen/android/app/src/main/jniLibs/$abi"
mkdir -p "$output"
go build -p 4 -buildmode=c-shared -trimpath -ldflags='-s -w -extldflags=-Wl,-z,max-page-size=16384' -o "$output/libneige_p2p.so" .
