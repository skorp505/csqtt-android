#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

echo "=== CSQTT APK Build Script ==="
echo "=== Output: 4 APKs (universal, arm64-v8a, armeabi-v7a, x86_64) ==="
echo ""

MISSING=0
if [[ ! -f "app/src/main/jniLibs/arm64-v8a/libclient.so" ]]; then
    echo "ERROR: arm64-v8a .so not found!"
    MISSING=1
fi
if [[ ! -f "app/src/main/jniLibs/armeabi-v7a/libclient.so" ]]; then
    echo "ERROR: armeabi-v7a .so not found!"
    MISSING=1
fi
if [[ ! -f "app/src/main/jniLibs/x86_64/libclient.so" ]]; then
    echo "ERROR: x86_64 .so not found!"
    MISSING=1
fi
if [[ "$MISSING" == "1" ]]; then
    echo ""
    echo "Run scripts/build_android_native.sh first to build and verify all native libraries!"
    exit 1
fi
for server_asset in csqtt-linux-amd64 csqtt-linux-arm64 csqtt-linux-armv7; do
    if [[ ! -f "app/src/main/assets/$server_asset" ]]; then
        echo "ERROR: embedded server app/src/main/assets/$server_asset not found!" >&2
        echo "Run rust-server/build_linux.sh --arch all and copy rust-server/dist/csqtt-linux-* into app/src/main/assets/." >&2
        exit 1
    fi
done
python3 scripts/native_client_provenance.py verify

echo "Incremental build..."
echo "Building release APKs..."

SDK_DIR=""
if [[ -n "${ANDROID_HOME:-}" && -d "$ANDROID_HOME" ]]; then
  SDK_DIR="$ANDROID_HOME"
elif [[ -n "${ANDROID_SDK_ROOT:-}" && -d "$ANDROID_SDK_ROOT" ]]; then
  SDK_DIR="$ANDROID_SDK_ROOT"
elif [[ -f "local.properties" ]]; then
  SDK_DIR="$(grep -E '^sdk\.dir=' local.properties | head -n1 | cut -d= -f2- || true)"
  SDK_DIR="${SDK_DIR//\\:/:}"
  if [[ -n "$SDK_DIR" && ! -d "$SDK_DIR" ]]; then
    SDK_DIR=""
  fi
fi

if [[ -z "$SDK_DIR" ]]; then
  for cand in \
    "$HOME/AppData/Local/Android/Sdk" \
    "$HOME/Library/Android/sdk" \
    "/opt/android-sdk" \
    "/usr/lib/android-sdk"; do
    if [[ -d "$cand" ]]; then
      SDK_DIR="$cand"
      break
    fi
  done
fi

if [[ -n "$SDK_DIR" ]]; then
  export ANDROID_HOME="$SDK_DIR"
  echo "Using SDK: $SDK_DIR"
fi

if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
    ./gradlew.bat assembleRelease --no-daemon
else
    bash gradlew assembleRelease --no-daemon
fi

mkdir -p app/release

echo ""
echo "Copying APKs to release folder..."

APK_DIR="app/build/outputs/apk/release"

if [[ -f "$APK_DIR/app-universal-release.apk" ]]; then
    cp "$APK_DIR/app-universal-release.apk" "app/release/CSQTT-universal.apk"
    echo "  [OK] CSQTT-universal.apk"
else
    echo "  [!!] Universal APK not found"
fi

if [[ -f "$APK_DIR/app-arm64-v8a-release.apk" ]]; then
    cp "$APK_DIR/app-arm64-v8a-release.apk" "app/release/CSQTT-arm64-v8a.apk"
    echo "  [OK] CSQTT-arm64-v8a.apk"
else
    echo "  [!!] arm64-v8a APK not found"
fi

if [[ -f "$APK_DIR/app-armeabi-v7a-release.apk" ]]; then
    cp "$APK_DIR/app-armeabi-v7a-release.apk" "app/release/CSQTT-armeabi-v7a.apk"
    echo "  [OK] CSQTT-armeabi-v7a.apk"
else
    echo "  [!!] armeabi-v7a APK not found"
fi

if [[ -f "$APK_DIR/app-x86_64-release.apk" ]]; then
    cp "$APK_DIR/app-x86_64-release.apk" "app/release/CSQTT-x86_64.apk"
    echo "  [OK] CSQTT-x86_64.apk"
else
    echo "  [!!] x86_64 APK not found"
fi

echo ""
echo "=== DONE ==="
echo "Output directory: app/release/"
echo ""
echo "  CSQTT-universal.apk    - all architectures in one APK"
echo "  CSQTT-arm64-v8a.apk    - 64-bit ARM only"
echo "  CSQTT-armeabi-v7a.apk  - 32-bit ARM only"
echo "  CSQTT-x86_64.apk       - x86_64 only (emulators, Chromebook, TV boxes)"
echo ""
