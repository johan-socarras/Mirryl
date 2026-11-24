# Builds Mirryl-User-Guide.pdf (shipped beside the app, opened from About) from
# guide.html and the pictures in img/. -Capture takes the pictures again first
# (capture.mjs: the app's own page with example phones, in a headless Edge).
# Needs Node and Microsoft Edge.
param([switch]$Capture)

$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
if ($Capture) {
    Push-Location $here
    try { node capture.mjs; if ($LASTEXITCODE -ne 0) { throw "capture.mjs failed" } } finally { Pop-Location }
}
$edge = "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe"
if (-not (Test-Path -LiteralPath $edge)) { $edge = "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe" }
$out = (Join-Path $here "..\src-tauri\Mirryl-User-Guide.pdf")
$out = [System.IO.Path]::GetFullPath($out)
$profile = Join-Path $env:TEMP "mirryl-guide-pdf"
$url = "file:///" + ((Join-Path $here "guide.html") -replace '\\', '/')
if (Test-Path -LiteralPath $out) { Remove-Item -LiteralPath $out -Force }
& $edge --headless=new --disable-gpu "--user-data-dir=$profile" --no-pdf-header-footer "--print-to-pdf=$out" $url | Out-Null
for ($i = 0; $i -lt 60 -and -not (Test-Path -LiteralPath $out); $i++) { Start-Sleep -Milliseconds 500 }
if (-not (Test-Path -LiteralPath $out)) { throw "Edge did not write $out" }
Get-Item -LiteralPath $out | Select-Object FullName, Length, LastWriteTime
