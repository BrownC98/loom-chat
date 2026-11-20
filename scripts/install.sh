#!/bin/sh
set -eu

bin=$HOME/loom-chat/bin/loom
data=$HOME/.loom-chat
port=${LOOM_PORT:-7425}
raw=https://raw.githubusercontent.com/BrownC98/loom-chat/main
releases=https://github.com/BrownC98/loom-chat/releases

is_web_install() {
  [ -x "$bin" ] && [ "$(head -c 2 "$bin")" != '#!' ]
}

point_bin_to() {
  mkdir -p "$(dirname "$bin")"
  if [ -n "${WSL_DISTRO_NAME:-}" ]; then
    printf '#!/bin/sh\ncase $1 in serve|start|stop|open) [ "$HOME" = '"'%s'"' ] || { echo "loom: this command would act on the human'"'"'s Windows daemon; build a Linux test binary instead (see AGENTS.md \\"데몬 띄우기(시험)\\")" >&2; exit 2; } ;; esac\nexec "%s" "$@"\n' "$(printf %s "$HOME" | sed "s/'/'\\\\''/g")" "$1"
  else
    printf '#!/bin/sh\nexec "%s" "$@"\n' "$1"
  fi >"$bin.new"
  chmod +x "$bin.new"
  mv "$bin.new" "$bin"
  echo "loom 명령이 $1을 부릅니다: $bin"
}

if [ -n "${WSL_DISTRO_NAME:-}" ]; then
  carry=
  if is_web_install; then
    "$bin" stop
    carry=$data
  fi
  rm -f "$data/serve.pid"
  export WSLENV="${WSLENV:+$WSLENV:}LOOM_DOWNLOAD_URL:LOOM_DESKTOP/p"
  windows_exe=$(
    cd /mnt/c
    powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "
      \$ErrorActionPreference = 'Stop'
      [Console]::OutputEncoding = [Text.Encoding]::UTF8
      irm $raw/scripts/install.ps1 | iex | ForEach-Object { [Console]::Error.WriteLine(\$_) }
      [Console]::WriteLine(\$shortcut.TargetPath)"
  )
  exe=$(wslpath "$(printf '%s' "$windows_exe" | tr -d '\r')")
  point_bin_to "$exe"
  if [ -n "$carry" ]; then "$bin" carry "$carry"; fi
  exit 0
fi

if [ "$(uname -s)" = Darwin ]; then
  if [ "$(uname -m)" != arm64 ]; then
    echo "macOS 앱은 Apple Silicon만 있습니다." >&2
    exit 1
  fi
  url=${LOOM_DOWNLOAD_URL:-}
  if [ -z "$url" ]; then
    version=$(curl -fsSL "$releases/latest/download/latest.json" | sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p')
    if [ -z "$version" ]; then
      echo "가장 새 판을 찾지 못했습니다: $releases/latest/download/latest.json" >&2
      exit 1
    fi
    url=$releases/download/v$version/loom-chat_${version}_aarch64.app.tar.gz
  fi
  dir=/Applications
  [ -w "$dir" ] || dir=$HOME/Applications
  mkdir -p "$dir"
  tmp=$(mktemp -d "$dir/.loom-chat.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT
  curl -fL --progress-bar -o "$tmp/loom-chat.app.tar.gz" "$url"
  tar -xzf "$tmp/loom-chat.app.tar.gz" -C "$tmp"
  xattr -cr "$tmp/loom-chat.app"
  with_desktop=
  desktop_files=$tmp/loom-desktop-macos-arm64
  if curl -fsL -o "$tmp/desktop.tar.gz" "${url%/*}/loom-desktop-macos-arm64.tar.gz" && tar -xzf "$tmp/desktop.tar.gz" -C "$tmp" && [ -x "$desktop_files/loom-desktop" ]; then
    with_desktop=1
    macos=$tmp/loom-chat-desktop.app/Contents/MacOS
    mkdir -p "$macos"
    mv "$desktop_files/loom-desktop" "$macos/"
    legacy_cli=$dir/loom-chat-desktop.app/Contents/MacOS/loom
    if [ -x "$desktop_files/loom" ]; then
      mv "$desktop_files/loom" "$macos/"
    elif [ -x "$legacy_cli" ]; then
      cp -p "$legacy_cli" "$macos/"
    fi
    mkdir "$tmp/loom-chat-desktop.app/Contents/Resources"
    mv "$desktop_files/icon.icns" "$tmp/loom-chat-desktop.app/Contents/Resources/" 2>/dev/null || true
    cat >"$tmp/loom-chat-desktop.app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>loom-desktop</string>
<key>CFBundleIconFile</key><string>icon</string>
<key>CFBundleIdentifier</key><string>com.brownc98.loom-chat.desktop</string>
<key>CFBundleName</key><string>loom-chat</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
    xattr -cr "$tmp/loom-chat-desktop.app"
  else
    echo "이 릴리스에는 데스크톱 앱이 없습니다. 지금까지의 loom 앱만 설치합니다."
  fi
  me=$(id -u)
  app_pids() {
    exe_path=$dir/$1/Contents/MacOS/$2
    ps -ww -U "$me" -o pid=,command= | LOOM_GUI_EXE="$exe_path" awk '
      { pid = $1; sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "") }
      $0 == ENVIRON["LOOM_GUI_EXE"] || $0 == ENVIRON["LOOM_GUI_EXE"] " --update" { print pid }
    '
  }
  quit_app() {
    pids=$("$@" || true)
    [ -n "$pids" ] || return 0
    printf '%s\n' "$pids" | xargs kill 2>/dev/null || true
    deadline=$(($(date +%s) + 10))
    while [ -n "$("$@" || true)" ]; do
      if [ "$(date +%s)" -ge "$deadline" ]; then
        echo "loom 앱이 끝나지 않았습니다. ⌘Q로 앱을 끝내고 다시 실행합니다." >&2
        exit 1
      fi
      sleep 0.2
    done
  }
  put_app() {
    if [ -e "$dir/$1" ]; then mv "$dir/$1" "$tmp/old-$1"; fi
    if ! mv "$tmp/$1" "$dir/$1"; then
      if [ -e "$tmp/old-$1" ] && ! mv "$tmp/old-$1" "$dir/$1"; then
        trap - EXIT
        echo "옛 $1을 되돌리지 못했습니다. 옛 앱은 여기에 있습니다: $tmp/old-$1" >&2
      fi
      exit 1
    fi
  }
  quit_app app_pids loom-chat.app loom-app
  put_app loom-chat.app
  if [ -n "$with_desktop" ]; then
    quit_app app_pids loom-chat-desktop.app loom-desktop
    put_app loom-chat-desktop.app
    echo "loom 데스크톱 앱을 설치했습니다: $dir/loom-chat-desktop.app"
  fi
  echo "loom 앱을 설치했습니다: $dir/loom-chat.app"
  exe=$dir/loom-chat.app/Contents/MacOS/loom
  if is_web_install; then
    "$bin" stop
    point_bin_to "$exe"
  fi
  launcher="$HOME/Applications/loom-chat (browser).command"
  mkdir -p "$HOME/Applications"
  printf '#!/bin/sh\nexec "%s" open\n' "$exe" >"$launcher"
  chmod +x "$launcher"
  echo "브라우저로 여는 loom을 만들었습니다: $launcher"
  if [ -n "$with_desktop" ]; then open "$dir/loom-chat-desktop.app"; else open "$dir/loom-chat.app"; fi
  exit 0
fi

if [ "$(uname -s)/$(uname -m)" != Linux/x86_64 ]; then
  echo "이 스크립트는 x86-64 Linux·WSL과 Apple Silicon macOS용입니다. Windows는 PowerShell에서 'irm $raw/scripts/install.ps1 | iex'를 실행하거나 데스크톱 앱을 씁니다: $releases/latest" >&2
  exit 1
fi

url=${LOOM_DOWNLOAD_URL:-$releases/latest/download/loom-web-linux-x64}
mkdir -p "$(dirname "$bin")" "$data"

curl -fL --progress-bar -o "$bin.new" "$url"
chmod +x "$bin.new"
mv "$bin.new" "$bin"

curl -fsL -o "$data/icon.png" "$raw/app/icons/icon.png" || rm -f "$data/icon.png"
apps=$HOME/.local/share/applications
mkdir -p "$apps"
cat >"$apps/loom-chat.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=loom-chat
Comment=Open loom-chat in the browser
Exec=$bin open --listen 127.0.0.1:$port
Icon=$data/icon.png
Terminal=false
Categories=Development;
EOF
desktop=${LOOM_DESKTOP:-$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")}
if [ -d "$desktop" ]; then
  cp "$apps/loom-chat.desktop" "$desktop/loom-chat.desktop"
  chmod +x "$desktop/loom-chat.desktop"
  gio set "$desktop/loom-chat.desktop" metadata::trusted true 2>/dev/null || true
fi
echo "앱 메뉴에 loom-chat을 더했습니다: $apps/loom-chat.desktop"

pidfile=$data/serve.pid
if pid=$(cat "$pidfile" 2>/dev/null) && tr '\0' ' ' 2>/dev/null <"/proc/$pid/cmdline" | grep -q "^$bin serve "; then
  kill "$pid"
  while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
fi
rm -f "$pidfile"
"$bin" open --listen "127.0.0.1:$port"
