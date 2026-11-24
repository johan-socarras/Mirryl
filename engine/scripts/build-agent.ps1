param(
    [string]$AndroidSdk = "",
    [string]$Platform = "",
    [string]$BuildTools = "",
    [string]$Output = ""
)

$ErrorActionPreference = "Stop"
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

function Convert-ToSortableVersion([string]$Value) {
    $parts = @($Value.Split("."))
    while ($parts.Count -lt 3) {
        $parts += "0"
    }
    return [version](($parts[0..2]) -join ".")
}

if ([string]::IsNullOrWhiteSpace($AndroidSdk)) {
    if ($env:ANDROID_SDK_ROOT) {
        $AndroidSdk = $env:ANDROID_SDK_ROOT
    } elseif ($env:ANDROID_HOME) {
        $AndroidSdk = $env:ANDROID_HOME
    } else {
        $AndroidSdk = Join-Path $env:LOCALAPPDATA "Android\Sdk"
    }
}
$AndroidSdk = (Resolve-Path -LiteralPath $AndroidSdk).Path

if ([string]::IsNullOrWhiteSpace($Platform)) {
    $Platform = Get-ChildItem -LiteralPath (Join-Path $AndroidSdk "platforms") -Directory |
        Sort-Object {
            Convert-ToSortableVersion ($_.Name -replace "^android-", "")
        } -Descending |
        Select-Object -First 1 -ExpandProperty Name
}
if ([string]::IsNullOrWhiteSpace($BuildTools)) {
    $BuildTools = Get-ChildItem -LiteralPath (Join-Path $AndroidSdk "build-tools") -Directory |
        Sort-Object { Convert-ToSortableVersion $_.Name } -Descending |
        Select-Object -First 1 -ExpandProperty Name
}
if ([string]::IsNullOrWhiteSpace($Output)) {
    $Output = Join-Path $projectRoot "assets\mirryl-stream-agent.jar"
}

$androidJar = Join-Path $AndroidSdk "platforms\$Platform\android.jar"
$d8 = Join-Path $AndroidSdk "build-tools\$BuildTools\d8.bat"
if (-not (Test-Path -LiteralPath $androidJar -PathType Leaf)) {
    throw "android.jar not found: $androidJar"
}
if (-not (Test-Path -LiteralPath $d8 -PathType Leaf)) {
    throw "d8.bat not found: $d8"
}

$javac = (Get-Command javac.exe -ErrorAction Stop).Source
$jar = (Get-Command jar.exe -ErrorAction Stop).Source
$sourceRoot = Join-Path $projectRoot "agent\src"
$sourceFiles = @(
    Get-ChildItem -LiteralPath $sourceRoot -Recurse -File -Filter "*.java" |
        Select-Object -ExpandProperty FullName
)
if ($sourceFiles.Count -eq 0) {
    throw "No se encontraron fuentes Java en $sourceRoot"
}

$buildRoot = Join-Path $projectRoot "build\android-agent"
$classes = Join-Path $buildRoot "classes"
$dex = Join-Path $buildRoot "dex"
if (Test-Path -LiteralPath $buildRoot) {
    $resolvedBuild = (Resolve-Path -LiteralPath $buildRoot).Path
    $expectedPrefix = (Join-Path $projectRoot "build")
    if (-not $resolvedBuild.StartsWith($expectedPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Ruta de limpieza inesperada: $resolvedBuild"
    }
    Remove-Item -LiteralPath $resolvedBuild -Recurse -Force
}
New-Item -ItemType Directory -Path $classes, $dex | Out-Null

& $javac -encoding UTF-8 -source 8 -target 8 -classpath $androidJar -d $classes @sourceFiles
if ($LASTEXITCODE -ne 0) {
    throw "javac exited with code $LASTEXITCODE"
}

$classFiles = @(
    Get-ChildItem -LiteralPath $classes -Recurse -File -Filter "*.class" |
        Select-Object -ExpandProperty FullName
)
& $d8 --min-api 30 --output $dex @classFiles
if ($LASTEXITCODE -ne 0) {
    throw "D8 exited with code $LASTEXITCODE"
}

$outputDirectory = Split-Path -Parent $Output
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
if (Test-Path -LiteralPath $Output) {
    Remove-Item -LiteralPath $Output -Force
}
& $jar --create --file $Output -C $dex classes.dex
if ($LASTEXITCODE -ne 0) {
    throw "jar exited with code $LASTEXITCODE"
}

Get-Item -LiteralPath $Output | Select-Object FullName, Length, LastWriteTime
