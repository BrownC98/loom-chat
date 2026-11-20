$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$url = if ($env:LOOM_DOWNLOAD_URL) { $env:LOOM_DOWNLOAD_URL } else { 'https://github.com/BrownC98/loom-chat/releases/latest/download/loom-web-windows-x64.exe' }
$linux_url = $url -replace 'loom-web-windows-x64\.exe$', 'loom-web-linux-x64'
$desktop_url = $url -replace 'loom-web-windows-x64\.exe$', 'loom-desktop-windows-x64.zip'
$raw = 'https://raw.githubusercontent.com/BrownC98/loom-chat/main'
$home_dir = Join-Path $env:LOCALAPPDATA 'loom'
$dir = Join-Path $home_dir 'bin'
$exe = Join-Path $dir 'loom.exe'
$app = Join-Path $dir 'loom-desktop.exe'
$linux = Join-Path $dir 'loom-linux'
$unpack = Join-Path $home_dir 'desktop.new'
New-Item -ItemType Directory -Force -Path $dir | Out-Null

function Install-File($from, $to) {
  if (Test-Path $to) {
    try {
      Remove-Item $to -Force
    } catch {
      Rename-Item $to "$(Split-Path $to -Leaf).$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds()).old"
    }
  }
  Move-Item $from $to
}

Get-ChildItem -Path $dir -Filter '*.exe.*.old' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
Remove-Item $unpack -Recurse -Force -ErrorAction SilentlyContinue
$with_desktop = $false
$with_embedded = $false
if ($desktop_url -ne $url) {
  try {
    Invoke-WebRequest -Uri $desktop_url -OutFile "$unpack.zip" -UseBasicParsing
    Expand-Archive -Path "$unpack.zip" -DestinationPath $unpack -Force
    $bundled_app = Join-Path $unpack 'loom-desktop.exe'
    $with_embedded = Test-Path $bundled_app
    if (!$with_embedded) { $bundled_app = Join-Path $unpack '*\loom-desktop.exe' }
    $with_desktop = $with_embedded -or (Test-Path $bundled_app)
  } catch {
    $with_desktop = $false
  }
  Remove-Item "$unpack.zip" -Force -ErrorAction SilentlyContinue
}
$with_web = !$with_desktop
if ($with_desktop) {
  if (!$with_embedded) {
    $web = Join-Path $unpack '*\loom.exe'
    $with_web = Test-Path $web
    if ($with_web) { Move-Item -Force $web "$exe.new" }
  }
  Move-Item -Force $bundled_app "$app.new"
} else {
  Write-Output '이 릴리스에는 데스크톱 앱이 없습니다. 브라우저 판 loom만 설치합니다.'
  Invoke-WebRequest -Uri $url -OutFile "$exe.new" -UseBasicParsing
}
$bundled_linux = Join-Path $unpack '*\loom-linux'
$with_linux = !$with_embedded -and $linux_url -ne $url
if (!$with_embedded -and $with_desktop -and (Test-Path $bundled_linux)) {
  Move-Item -Force $bundled_linux "$linux.new"
  $with_linux = $true
} elseif ($with_linux) {
  Invoke-WebRequest -Uri $linux_url -OutFile "$linux.new" -UseBasicParsing
}
Remove-Item $unpack -Recurse -Force -ErrorAction SilentlyContinue

if ($with_linux) { Move-Item -Force "$linux.new" $linux }
if ($with_web) { Install-File "$exe.new" $exe }
if ($with_desktop) {
  Install-File "$app.new" $app
  if ($with_embedded -and !(Test-Path -LiteralPath $exe) -and (Test-Path -LiteralPath $linux -PathType Leaf)) {
    Remove-Item -LiteralPath $linux -Force -ErrorAction SilentlyContinue
  }
}

$icon = Join-Path $home_dir 'icon.ico'
try { Invoke-WebRequest -Uri "$raw/app/icons/icon.ico" -OutFile $icon -UseBasicParsing } catch { $icon = $null }

$desktop = if ($env:LOOM_DESKTOP) { $env:LOOM_DESKTOP } else { [Environment]::GetFolderPath('Desktop') }
$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $desktop 'loom.lnk'))
if ($with_desktop) {
  $shortcut.TargetPath = $app
  $shortcut.Arguments = ''
  $shortcut.WindowStyle = 1
} else {
  $shortcut.TargetPath = $exe
  $shortcut.Arguments = 'open'
  $shortcut.WindowStyle = 7
}
$shortcut.Description = 'loom'
if ($icon) { $shortcut.IconLocation = $icon }
$shortcut.Save()
Write-Output "바탕 화면에 loom 아이콘을 만들었습니다: $(Join-Path $desktop 'loom.lnk')"

if ($with_desktop) { Start-Process $app } else { & $exe open }
