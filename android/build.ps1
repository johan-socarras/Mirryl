# Builds the Mirryl APK on Windows.
# Finds the installed build-tools and platform versions by itself.
$ErrorActionPreference = "Stop"

$ROOT = $PSScriptRoot
$PKG = "com.mirryl.app"

# --- Find the SDK ---
$SDK = $env:ANDROID_HOME
if (-not $SDK) { $SDK = $env:ANDROID_SDK_ROOT }
if (-not $SDK) { $SDK = "$env:LOCALAPPDATA\Android\Sdk" }
if (-not (Test-Path $SDK)) {
    throw "Android SDK not found. Set ANDROID_HOME or install it."
}

# Sort key by VERSION, not by string, like build.sh's `sort -V`: as text,
# "9.0.0" sorts after "36.0.0", and the two scripts could build the same APK
# with different build-tools on the same machine. Parity between the two scripts is
# precisely what makes the Windows APK and the Linux APK the same artifact.
# Platforms are not named with a pure version ("android-36.1"), so the numeric
# part is extracted; anything with no numbers at all goes to the end with
# [version]"0.0" instead of breaking the build.
function Get-SdkVersionKey {
    param([string]$Name)
    $m = [regex]::Match($Name, '\d+(?:\.\d+)*')
    if (-not $m.Success) { return [version]"0.0" }
    # [version] needs 2 to 4 components: neither "36" alone nor "1.2.3.4.5"
    # parses.
    $parts = @($m.Value -split '\.' | Select-Object -First 4)
    while ($parts.Count -lt 2) { $parts += "0" }
    try { return [version]($parts -join '.') } catch { return [version]"0.0" }
}

# --- Take the newest build-tools and platform ---
$BT = Get-ChildItem "$SDK\build-tools" -Directory -ErrorAction SilentlyContinue |
      Sort-Object { Get-SdkVersionKey $_.Name } -Descending | Select-Object -First 1
if (-not $BT) { throw "No build-tools installed in $SDK\build-tools" }
$BT = $BT.FullName

$PLATFORM = Get-ChildItem "$SDK\platforms" -Directory -ErrorAction SilentlyContinue |
            Sort-Object { Get-SdkVersionKey $_.Name } -Descending |
            ForEach-Object { Join-Path $_.FullName "android.jar" } |
            Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $PLATFORM) { throw "No platform with android.jar in $SDK\platforms" }

Write-Host "SDK:       $SDK"
Write-Host "build-tools: $BT"
Write-Host "platform:    $PLATFORM"
Write-Host ""

$BUILD = "$ROOT\build"
$GEN = "$BUILD\gen"
$OBJ = "$BUILD\obj"
$DEXOUT = "$BUILD\dex"
$RESZIP = "$BUILD\compiled_res.zip"
$BASEAPK = "$BUILD\base.apk"
$ALIGNEDAPK = "$BUILD\aligned.apk"
$SIGNEDAPK = "$BUILD\Mirryl.apk"
# The key that signs the APK. It lives OUTSIDE the project tree, and outside
# any folder synced to the cloud: it is the only key that can sign an update
# that phones which already have the app will accept. Lose it (or its
# password) and none of them can be updated again.
#
# The keystore is created once with create-keystore.ps1: the phone app is not
# on Google Play (so there is no Play App Signing), the PC app installs it, and
# the key of the first public APK is fixed for good. MIRRYL_KEYSTORE and
# MIRRYL_KEYSTORE_ALIAS point the build at another keystore.
$KEYSTORE = $env:MIRRYL_KEYSTORE
if (-not $KEYSTORE) { $KEYSTORE = "$env:USERPROFILE\Mirryl-keys\mirryl-phone-app.keystore" }
$KEYALIAS = $env:MIRRYL_KEYSTORE_ALIAS
if (-not $KEYALIAS) { $KEYALIAS = "mirryl" }
# Next to the signed APK its versionCode is written, read from the APK itself:
# it is what the PC app packages with it (build.rs) to know which version it carries.
$VERSIONFILE = "$BUILD\Mirryl.apk.version"

Write-Host "== Cleaning the previous build ==" -ForegroundColor Cyan
Remove-Item -Recurse -Force $BUILD -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $GEN, $OBJ, $DEXOUT | Out-Null

Write-Host "== 1. Compiling resources (aapt2 compile) ==" -ForegroundColor Cyan
& "$BT\aapt2.exe" compile --dir "$ROOT\res" -o $RESZIP
if ($LASTEXITCODE -ne 0) { throw "aapt2 compile failed" }

Write-Host "== 2. Linking resources + manifest (aapt2 link) ==" -ForegroundColor Cyan
& "$BT\aapt2.exe" link -o $BASEAPK -I $PLATFORM `
    --manifest "$ROOT\AndroidManifest.xml" `
    -R $RESZIP `
    --java $GEN `
    --min-sdk-version 30 `
    --target-sdk-version 36 `
    --auto-add-overlay
if ($LASTEXITCODE -ne 0) { throw "aapt2 link failed" }

Write-Host "== 3. Compiling Java (javac) ==" -ForegroundColor Cyan
$javaFiles = Get-ChildItem -Recurse -Path $GEN, "$ROOT\src" -Filter *.java | ForEach-Object { $_.FullName.Replace('\', '/') }
$sourcesFile = "$BUILD\sources.txt"
$javaFiles | ForEach-Object { "`"$_`"" } | Set-Content $sourcesFile
javac -encoding UTF-8 -source 8 -target 8 -bootclasspath $PLATFORM -classpath $PLATFORM -d $OBJ "@$sourcesFile"
if (-not (Test-Path "$OBJ\com\mirryl\app\MainActivity.class")) { throw "javac failed" }

Write-Host "== 4. Converting to dex (d8) ==" -ForegroundColor Cyan
$classFiles = Get-ChildItem -Recurse -Path $OBJ -Filter *.class | ForEach-Object { $_.FullName }
& "$BT\d8.bat" --release --output $DEXOUT --lib $PLATFORM $classFiles
if ($LASTEXITCODE -ne 0) { throw "d8 failed" }

Write-Host "== 5. Packing classes.dex into the APK ==" -ForegroundColor Cyan
Copy-Item $BASEAPK $ALIGNEDAPK -Force
Push-Location $DEXOUT
try {
    jar uf $ALIGNEDAPK classes.dex
    if ($LASTEXITCODE -ne 0) { throw "jar (packing the dex) failed" }
} finally {
    Pop-Location
}

Write-Host "== 6. Aligning the APK (zipalign) ==" -ForegroundColor Cyan
$alignedFinal = "$BUILD\aligned_final.apk"
& "$BT\zipalign.exe" -f -p 4 $ALIGNEDAPK $alignedFinal
if ($LASTEXITCODE -ne 0) { throw "zipalign failed" }

Write-Host "== 7. Signing key ==" -ForegroundColor Cyan
if (-not (Test-Path $KEYSTORE)) {
    # Never a NEW key: an APK signed with one looks right, and NO phone that
    # already has the app can accept it as an update -- silently, and with no
    # way back once published. Minting a signing key can never be a build's
    # default behaviour: if it is missing, the build stops.
    throw @"
Signing key not found:
  $KEYSTORE

This is the RELEASE key. Without it NO update can be published that phones
which already have the app will accept, and generating a new one is NOT a
fix: it breaks all of them for good.

If you moved it, point to it with the MIRRYL_KEYSTORE variable.
If it does not exist yet, create it ONCE with
  .\create-keystore.ps1
If it is really lost, this is an incident, not a build hiccup.
"@
}
Write-Host "   $KEYSTORE (alias $KEYALIAS)"

Write-Host "== 8. Signing the APK (apksigner) ==" -ForegroundColor Cyan
# The keystore password is not written here. apksigner reads it from the
# MIRRYL_KEYSTORE_PASS environment variable (it serves for the keystore and
# for the key, which share a password); if it is not set, it is asked for at
# the keyboard without echo and lasts only for this process. build.sh uses
# the same variable.
if (-not $env:MIRRYL_KEYSTORE_PASS) {
    $secure = Read-Host -AsSecureString "Keystore password ($KEYSTORE)"
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try { $env:MIRRYL_KEYSTORE_PASS = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr) }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr) }
}
if (-not $env:MIRRYL_KEYSTORE_PASS) { throw "No keystore password (MIRRYL_KEYSTORE_PASS)" }
& "$BT\apksigner.bat" sign --ks $KEYSTORE --ks-pass env:MIRRYL_KEYSTORE_PASS --ks-key-alias $KEYALIAS `
    --key-pass env:MIRRYL_KEYSTORE_PASS --out $SIGNEDAPK $alignedFinal
if ($LASTEXITCODE -ne 0) { throw "apksigner failed" }

Write-Host "== 9. APK version, for the PC app ==" -ForegroundColor Cyan
# From the signed APK, not from the manifest: what is written is what is inside.
$badging = & "$BT\aapt2.exe" dump badging $SIGNEDAPK 2>&1 | Out-String
$m = [regex]::Match($badging, "package: name='$PKG' versionCode='(\d+)'")
if (-not $m.Success) { throw "Could not read the versionCode of $SIGNEDAPK with aapt2 dump badging" }
Set-Content -Path $VERSIONFILE -Value $m.Groups[1].Value -Encoding ascii
Write-Host "   versionCode $($m.Groups[1].Value) -> $VERSIONFILE"

Write-Host ""
Write-Host "== DONE ==" -ForegroundColor Green
Write-Host "APK written to: $SIGNEDAPK"
