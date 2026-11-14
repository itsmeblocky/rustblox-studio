param(
    [string]$Configuration = "release",
    [string]$QtDir = "",
    [string]$WixBin = "",
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ([string]::IsNullOrWhiteSpace($QtDir)) {
    $QtDir = Join-Path $root ".tools\Qt\6.8.3\msvc2022_64"
}
$windeploy = Join-Path $QtDir "bin\windeployqt.exe"
if (!(Test-Path $windeploy)) { throw "windeployqt not found at $windeploy. Set -QtDir or install Qt." }

Write-Host "==> Building rbx-studio-app ($Configuration)"
$env:QTDIR = $QtDir
$env:PATH = "$(Join-Path $QtDir 'bin');$env:PATH"
if ($Configuration -eq "release") {
    cargo build -p rbx-studio-app --release -j 2
    $exeSrc = Join-Path $root "target\release\rbx-studio-app.exe"
} else {
    cargo build -p rbx-studio-app -j 2
    $exeSrc = Join-Path $root "target\debug\rbx-studio-app.exe"
}
if (!(Test-Path $exeSrc)) { throw "Built exe not found: $exeSrc" }

$stage = Join-Path $root "installer\staging"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stage "assets") | Out-Null

Write-Host "==> Staging executable and assets"
Copy-Item $exeSrc (Join-Path $stage "rbx-studio-app.exe") -Force
# Force the customizable Qt Quick Controls Basic style (the native Windows
# style ignores our dark-theme menu/scrollbar chrome).
Copy-Item (Join-Path $root "installer\qtquickcontrols2.conf") (Join-Path $stage "qtquickcontrols2.conf") -Force
Copy-Item (Join-Path $root "assets\*") (Join-Path $stage "assets") -Recurse -Force
# Secrets/credentials are per-user and must not be bundled
if (Test-Path (Join-Path $stage "assets\..\secrets")) { }

Write-Host "==> Deploying Qt runtime"
# QML is loaded from a string at runtime (qmetaobject), so provide an import hint file for windeployqt.
$qmlHintDir = Join-Path $env:TEMP "rustblox-windeploy-qml"
New-Item -ItemType Directory -Force -Path $qmlHintDir | Out-Null
@"
import QtQuick 2.15
import QtQuick.Window 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15
"@ | Set-Content -Encoding ASCII (Join-Path $qmlHintDir "Imports.qml")

& $windeploy --release --no-translations --compiler-runtime --qmldir $qmlHintDir --dir $stage (Join-Path $stage "rbx-studio-app.exe")
if ($LASTEXITCODE -ne 0) { throw "windeployqt failed ($LASTEXITCODE)" }

# qmetaobject apps need QtQuick/Controls/Layouts/Templates modules and their backing DLLs.
$qmlSrc = Join-Path $QtDir "qml"
if (Test-Path $qmlSrc) {
    Write-Host "==> Copying QML modules"
    New-Item -ItemType Directory -Force -Path (Join-Path $stage "qml") | Out-Null
    foreach ($mod in @("QtQuick","QtQuick.2","QtQml","QtGraphicalEffects","QtQml.Models","QtQml.WorkerScript")) {
        $m = Join-Path $qmlSrc $mod
        if (Test-Path $m) { Copy-Item $m (Join-Path $stage "qml") -Recurse -Force }
    }
}
# Ensure Quick Controls/Templates/Layouts runtime DLLs are present (windeployqt may miss them when QML is dynamic)
$qtBin = Join-Path $QtDir "bin"
foreach ($pat in @("Qt6QuickControls2*.dll","Qt6QuickTemplates2*.dll","Qt6QuickLayouts*.dll","Qt6QuickDialogs2*.dll","Qt6QuickDialogs2Utils*.dll","Qt6QuickEffects*.dll")) {
    Get-ChildItem (Join-Path $qtBin $pat) -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike "*d.dll" } | ForEach-Object {
        Copy-Item $_.FullName (Join-Path $stage $_.Name) -Force
    }
}

$pluginsSrc = Join-Path $QtDir "plugins"
foreach ($p in @("platforms","imageformats","styles","iconengines")) {
    $srcp = Join-Path $pluginsSrc $p
    $dstp = Join-Path $stage "plugins\$p"
    if (Test-Path $srcp) {
        New-Item -ItemType Directory -Force -Path (Join-Path $stage "plugins") | Out-Null
        if (!(Test-Path $dstp)) { Copy-Item $srcp $dstp -Recurse -Force }
    }
}
# qt.conf so installed app finds bundled plugins/qml
@"
[Paths]
Plugins = plugins
Imports = qml
Qml2Imports = qml
"@ | Set-Content -Encoding ASCII (Join-Path $stage "qt.conf")

Write-Host "==> Locating WiX tools"
function Find-Wix {
    param([string]$Bin)
    if ($Bin -and (Test-Path (Join-Path $Bin "candle.exe"))) { return $Bin }
    if ($Bin -and (Test-Path (Join-Path $Bin "wix.exe"))) { return $Bin }
    $cands = @(
        "C:\Program Files (x86)\WiX Toolset v3.14\bin",
        "C:\Program Files (x86)\WiX Toolset v3.11\bin",
        "C:\Program Files\WiX Toolset v3.14\bin",
        "C:\Program Files\WiX Toolset v3.11\bin"
    )
    foreach ($c in $cands) { if (Test-Path (Join-Path $c "candle.exe")) { return $c } }
    $wix4 = Get-Command wix.exe -ErrorAction SilentlyContinue
    if ($wix4) { return (Split-Path $wix4.Source) }
    $dotnetWix = Join-Path $env:USERPROFILE ".dotnet\tools\wix.exe"
    if (Test-Path $dotnetWix) { return (Split-Path $dotnetWix) }
    return $null
}
$wix = Find-Wix $WixBin
if (!$wix) {
    Write-Warning "WiX Toolset not found. Staging completed at: $stage"
    Write-Warning "Install WiX (e.g. 'dotnet tool install --global wix' or WiX v3) and re-run to produce the .msi."
    Write-Host "STAGED=$stage"
    exit 0
}

$outDir = Join-Path $root "installer\out"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$msi = Join-Path $outDir "RustbloxStudio-$Version.msi"
$wxs = Join-Path $root "installer\rustblox-studio.wxs"
$assetsWxs = Join-Path $root "installer\assets.wxs"
$objDir = Join-Path $root "installer\obj"
if (Test-Path $objDir) { Remove-Item -Recurse -Force $objDir }
New-Item -ItemType Directory -Force -Path $objDir | Out-Null

if (Test-Path (Join-Path $wix "candle.exe")) {
    $heat = Join-Path $wix "heat.exe"
    if (!(Test-Path $heat)) { throw "heat.exe not found in $wix" }
    Write-Host "==> Harvesting staged files"
    & $heat dir $stage -cg HarvestedFiles -dr INSTALLFOLDER -scom -sreg -srd -var var.StageDir -ag -out $assetsWxs
    if ($LASTEXITCODE -ne 0) { throw "heat failed ($LASTEXITCODE)" }
    Write-Host "==> Compiling MSI (WiX v3)"
    & (Join-Path $wix "candle.exe") -nologo -dStageDir=$stage -dProductVersion=$Version -out "$objDir\" $wxs $assetsWxs
    if ($LASTEXITCODE -ne 0) { throw "candle failed ($LASTEXITCODE)" }
    & (Join-Path $wix "light.exe") -nologo -ext WixUIExtension -out $msi "$objDir\rustblox-studio.wixobj" "$objDir\assets.wixobj"
    if ($LASTEXITCODE -ne 0) { throw "light failed ($LASTEXITCODE)" }
} else {
    Write-Host "==> Building MSI (WiX v4+/v7)"
    $wixExe = Join-Path $wix "wix.exe"
    # Ensure UI extension is available (best-effort)
    & $wixExe extension add WixToolset.UI.wixext 2>$null | Out-Null
    & $wixExe build -arch x64 -ext WixToolset.UI.wixext -d StageDir=$stage -d ProductVersion=$Version -o $msi $wxs
    if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
}

Write-Host "==> MSI created: $msi"
"MSI=$msi"