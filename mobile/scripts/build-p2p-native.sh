#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../p2p-native"
: "${NDK_HOME:?Set NDK_HOME to Android NDK29}"
export GOOS=android GOARCH=arm64 CGO_ENABLED=1
export CC="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android26-clang"
export CXX="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android26-clang++"
output=../src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a
mkdir -p "$output"
go build -p 4 -buildmode=c-shared -trimpath -ldflags='-s -w -extldflags=-Wl,-z,max-page-size=16384' -o "$output/libneige_p2p.so" .
