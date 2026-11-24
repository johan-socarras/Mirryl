# Builds mirryl-apps.jar, the small program the Apps screen runs on the phone
# (see src/com/mirryl/apps/AppListMain.java), into src-tauri/apps-agent/, where
# main.rs embeds it with include_bytes!. Run it after changing the Java, then
# build the app. Needs a JDK (javac, jar) and the Android SDK (android.jar, d8).
param(
    [string]$AndroidSdk = ""
)

$ErrorActionPreference = "Stop"
$here = $PSScriptRoot

if ([string]::IsNullOrWhiteSpace($AndroidSdk)) {
    $AndroidSdk = if ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT }
        elseif ($env:ANDROID_HOME) { $env:ANDROID_HOME }
        else { Join-Path $env:LOCALAPPDATA "Android\Sdk" }
}

function Get-Newest([string]$Folder, [string]$Prefix) {
    Get-ChildItem -LiteralPath $Folder -Directory |
        Where-Object { $_.Name -match "^$Prefix[0-9.]+$" } |
        Sort-Object { [version](($_.Name -replace "^$Prefix", "") + ".0.0" -replace '^(\d+\.\d+\.\d+).*', '$1') } -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

$platform = Get-Newest (Join-Path $AndroidSdk "platforms") "android-"
$buildTools = Get-Newest (Join-Path $AndroidSdk "build-tools") ""
$androidJar = Join-Path $platform "android.jar"
$d8 = Join-Path $buildTools "d8.bat"
foreach ($required in @($androidJar, $d8)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Not found: $required" }
}
$javac = (Get-Command javac.exe -ErrorAction Stop).Source
$jar = (Get-Command jar.exe -ErrorAction Stop).Source

$build = Join-Path $here "build"
if (Test-Path -LiteralPath $build) { Remove-Item -LiteralPath $build -Recurse -Force }
$classes = Join-Path $build "classes"
$dex = Join-Path $build "dex"
New-Item -ItemType Directory -Path $classes, $dex | Out-Null

$sources = @(Get-ChildItem -LiteralPath (Join-Path $here "src") -Recurse -File -Filter "*.java" | Select-Object -ExpandProperty FullName)
& $javac -encoding UTF-8 -source 8 -target 8 -nowarn -classpath $androidJar -d $classes @sources
if ($LASTEXITCODE -ne 0) { throw "javac exited with code $LASTEXITCODE" }

$classFiles = @(Get-ChildItem -LiteralPath $classes -Recurse -File -Filter "*.class" | Select-Object -ExpandProperty FullName)
& $d8 --min-api 30 --output $dex @classFiles
if ($LASTEXITCODE -ne 0) { throw "d8 exited with code $LASTEXITCODE" }

$output = Join-Path $here "..\src-tauri\apps-agent\mirryl-apps.jar"
New-Item -ItemType Directory -Path (Split-Path -Parent $output) -Force | Out-Null
if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output -Force }
& $jar --create --file $output -C $dex classes.dex
if ($LASTEXITCODE -ne 0) { throw "jar exited with code $LASTEXITCODE" }
Remove-Item -LiteralPath $build -Recurse -Force

Get-Item -LiteralPath $output | Select-Object FullName, Length, LastWriteTime
