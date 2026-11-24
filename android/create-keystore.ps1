# Creates, ONCE, the keystore build.ps1 signs the phone app's APK with.
#
# The phone app is not on Google Play (so there is no Play App Signing); the
# PC app carries it and installs it. The key of the first public APK is fixed
# for good, because Android does not let an app be updated with a different
# signature.
#
# After creating it: put the password in a password manager, and keep a copy
# of the file somewhere other than this PC (without the file and its password,
# no phone can ever be updated again).
$ErrorActionPreference = "Stop"

$KS = $env:MIRRYL_KEYSTORE
if (-not $KS) { $KS = "$env:USERPROFILE\Mirryl-keys\mirryl-phone-app.keystore" }
$ALIAS = $env:MIRRYL_KEYSTORE_ALIAS
if (-not $ALIAS) { $ALIAS = "mirryl" }

if (Test-Path $KS) {
    # Creating another one on top would leave installed phones unable to update.
    throw "A keystore already exists at $KS. Not creating another one."
}
# Check the real OneDrive folders, not the word: a folder whose name merely
# contains "OneDrive" (for example one named "...-outside-OneDrive") must not
# reject itself.
$ksFull = [IO.Path]::GetFullPath($KS)
foreach ($od in @($env:OneDrive, $env:OneDriveConsumer, $env:OneDriveCommercial)) {
    if ($od -and $ksFull.StartsWith($od.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "The keystore cannot live inside OneDrive ($od): $KS"
    }
}
New-Item -ItemType Directory -Force -Path (Split-Path $KS) | Out-Null

function Read-Secret([string]$Prompt) {
    $s = Read-Host -AsSecureString $Prompt
    $p = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($s)
    try { return [Runtime.InteropServices.Marshal]::PtrToStringBSTR($p) }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($p) }
}
$a = Read-Secret "New keystore password (at least 12 characters)"
if ($a.Length -lt 12) { throw "Too short." }
$b = Read-Secret "Repeat it"
if ($a -ne $b) { throw "They do not match." }
$env:MIRRYL_KEYSTORE_PASS = $a

# PKCS12: the key's password is the same as the store's, which is what
# build.ps1 expects (a single variable, MIRRYL_KEYSTORE_PASS).
& keytool -genkeypair -keystore $KS -storetype PKCS12 -alias $ALIAS `
    -keyalg RSA -keysize 4096 -validity 10000 `
    -dname "CN=Mirryl, O=Mirryl" `
    -storepass:env MIRRYL_KEYSTORE_PASS -keypass:env MIRRYL_KEYSTORE_PASS
if ($LASTEXITCODE -ne 0) { throw "keytool failed" }

Write-Host ""
Write-Host "Keystore created at: $KS (alias $ALIAS)" -ForegroundColor Green
Write-Host "Save the password in a password manager NOW, and keep a copy of the file somewhere other than this PC."
Write-Host "Then, in this same window: .\build.ps1 (it already has the password for this process)."
