#!/usr/bin/env bash
# All-in-1 Installer: build it on Linux (and macOS).
#   ./build.sh release   the program to hand out  -> public/release/all-in-1-installer (with Assets/)
#   ./build.sh dev       a debug build            -> public/dev/all-in-1-installer
#   ./build.sh run       a debug build, and start it
#   ./build.sh test      all tests
# Needs Rust (rustup.rs); on Linux also: sudo apt-get install -y pkg-config libxkbcommon-dev libgl1-mesa-dev libgtk-3-dev
set -euo pipefail
cd "$(dirname "$0")"
exe=all-in-1-installer
choice=${1:-release}

command -v cargo >/dev/null || { echo "Rust is not installed: install it from https://rustup.rs"; exit 1; }

case "$choice" in
  release)
    echo "> cargo build --release -p aio-app"
    cargo build --release -p aio-app
    mkdir -p public/release
    cp -f "target/release/$exe" "public/release/$exe"
    # The client images are in the program too; the folder next to it is what Deploy Assets reads first.
    rm -rf public/release/Assets
    cp -R source/Assets public/release/Assets
    echo "Ready: $PWD/public/release/$exe"
    ;;
  dev|run)
    echo "> cargo build -p aio-app"
    cargo build -p aio-app
    mkdir -p public/dev
    cp -f "target/debug/$exe" "public/dev/$exe"
    echo "Ready: $PWD/public/dev/$exe"
    if [ "$choice" = run ]; then (cd public/dev && nohup "./$exe" >/dev/null 2>&1 &); fi
    ;;
  test)
    echo "> cargo test --workspace"
    cargo test --workspace
    ;;
  *)
    echo "Unknown choice \"$choice\": use release, dev, run or test."
    exit 1
    ;;
esac
