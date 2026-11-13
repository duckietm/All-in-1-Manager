#!/usr/bin/env bash
# All-in-1 Manager: build it on Linux (and macOS).
#   ./build.sh release   the program to hand out  -> public/release/all-in-1-manager (with plugins/)
#   ./build.sh dev       a debug build            -> public/dev/all-in-1-manager
#   ./build.sh run       a debug build, and start it
#   ./build.sh test      all tests
# Release and dev also leave a link to the program here (./all-in-1-manager, ./all-in-1-manager-dev) and, on Linux,
# a launcher "All-in-1 Manager.desktop" to double-click.
# Needs Rust (rustup.rs); on Linux also: sudo apt-get install -y pkg-config libxkbcommon-dev libgl1-mesa-dev libgtk-3-dev
set -euo pipefail
cd "$(dirname "$0")"
exe=all-in-1-manager
choice=${1:-release}

# A link next to this script to the program, and on Linux a launcher to double-click in the file manager.
# The program starts in its own folder, so it keeps its own configuration/. A failure only warns.
# $1 = the folder of the program (public/release or public/dev), $2 = the link's name, $3 = the launcher's title.
shortcut() {
  local dir="$PWD/$1" link=$2 title=$3
  chmod +x "$dir/$exe" 2>/dev/null || true
  ln -sfn "$1/$exe" "$link" || echo "Note: the link $link could not be made; the program is in $1."
  if [ "$(uname -s)" = Linux ]; then
    local launcher="$title.desktop"
    {
      echo "[Desktop Entry]"
      echo "Type=Application"
      echo "Name=$title"
      echo "Comment=Hotel server setup"
      echo "Exec=\"$dir/$exe\""
      echo "Path=$dir"
      [ -f "$PWD/source/crates/app/assets/icon.png" ] && echo "Icon=$PWD/source/crates/app/assets/icon.png"
      echo "Terminal=false"
      echo "Categories=Utility;Development;"
    } > "$launcher" && chmod +x "$launcher" || echo "Note: the launcher $launcher could not be made."
    # GNOME's file manager only starts a launcher it trusts; without gio the user allows it once with a right click.
    command -v gio >/dev/null && gio set "$launcher" metadata::trusted true 2>/dev/null || true
  fi
}

command -v cargo >/dev/null || { echo "Rust is not installed: install it from https://rustup.rs"; exit 1; }

case "$choice" in
  release)
    echo "> cargo build --release -p aio-app"
    cargo build --release -p aio-app
    mkdir -p public/release
    cp -f "target/release/$exe" "public/release/$exe"
    # The data packs of the Plugins page, next to the program, so it needs no download.
    rm -rf public/release/plugins/client-assets
    mkdir -p public/release/plugins/default-catalog
    cp -R source/Assets public/release/plugins/client-assets
    cp -f source/crates/app/assets/catalog/Full_Catalog.zip public/release/plugins/default-catalog/
    shortcut public/release all-in-1-manager "All-in-1 Manager"
    echo "Ready: $PWD/public/release/$exe (link: ./all-in-1-manager)"
    ;;
  dev|run)
    echo "> cargo build -p aio-app"
    cargo build -p aio-app
    mkdir -p public/dev
    cp -f "target/debug/$exe" "public/dev/$exe"
    shortcut public/dev all-in-1-manager-dev "All-in-1 Manager (dev)"
    echo "Ready: $PWD/public/dev/$exe (link: ./all-in-1-manager-dev)"
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
