#!/usr/bin/env bash
# Linux port of build.ps1 — same steps, same RELEASE keystore, same output name.
# The signing key lives OUTSIDE the project tree; if it is missing, step 7
# aborts instead of minting a new one.
set -euo pipefail

# The script lives next to src/, res/ and AndroidManifest.xml.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SDK="${ANDROID_HOME:-$HOME/Android/Sdk}"

# Takes the newest build-tools and platform installed.
BT="$(ls -d "$SDK"/build-tools/*/ 2>/dev/null | sort -V | tail -1)"
BT="${BT%/}"
PLATFORM="$(ls "$SDK"/platforms/*/android.jar 2>/dev/null | sort -V | tail -1)"
[ -x "$BT/aapt2" ] || { echo "No build-tools in $SDK/build-tools"; exit 1; }
[ -f "$PLATFORM" ] || { echo "No platform with android.jar in $SDK/platforms"; exit 1; }
echo "SDK: $SDK"
echo "build-tools: $BT"
echo "platform: $PLATFORM"
echo

BUILD="$ROOT/build"
GEN="$BUILD/gen"
OBJ="$BUILD/obj"
DEXOUT="$BUILD/dex"
RESZIP="$BUILD/compiled_res.zip"
BASEAPK="$BUILD/base.apk"
ALIGNEDAPK="$BUILD/aligned.apk"
SIGNEDAPK="$BUILD/Mirryl.apk"
# The key that signs the APK. It lives OUTSIDE the project tree, and outside
# any folder synced to the cloud: it is the only key that can sign an update
# that phones which already have the app will accept. Lose it (or its
# password) and none of them can be updated again. Same lookup as build.ps1:
# $MIRRYL_KEYSTORE, else the fixed path.
#
# The keystore is created once with create-keystore.ps1: the phone app is not
# on Google Play (so there is no Play App Signing), the PC app installs it, and
# the key of the first public APK is fixed for good. MIRRYL_KEYSTORE and
# MIRRYL_KEYSTORE_ALIAS point the build at another keystore.
KEYSTORE="${MIRRYL_KEYSTORE:-$HOME/Mirryl-keys/mirryl-phone-app.keystore}"
KEYALIAS="${MIRRYL_KEYSTORE_ALIAS:-mirryl}"
# The signed APK's versionCode, read from the APK itself, written next to it:
# the PC app packages it with the APK (build.rs) to know which version it carries.
VERSIONFILE="$BUILD/Mirryl.apk.version"

echo "== Cleaning the previous build =="
rm -rf "$BUILD"
mkdir -p "$GEN" "$OBJ" "$DEXOUT"

echo "== 1. Compiling resources (aapt2 compile) =="
"$BT/aapt2" compile --dir "$ROOT/res" -o "$RESZIP"

echo "== 2. Linking resources + manifest (aapt2 link) =="
"$BT/aapt2" link -o "$BASEAPK" -I "$PLATFORM" \
    --manifest "$ROOT/AndroidManifest.xml" \
    -R "$RESZIP" \
    --java "$GEN" \
    --min-sdk-version 30 \
    --target-sdk-version 36 \
    --auto-add-overlay

echo "== 3. Compiling Java (javac) =="
find "$GEN" "$ROOT/src" -name '*.java' > "$BUILD/sources.txt"
javac -encoding UTF-8 -source 8 -target 8 \
    -bootclasspath "$PLATFORM" -classpath "$PLATFORM" \
    -d "$OBJ" "@$BUILD/sources.txt" 2>&1 | grep -v "^warning:\|obsolete\|deprecat" || true
[ -f "$OBJ/com/mirryl/app/MainActivity.class" ] || { echo "javac failed"; exit 1; }

echo "== 4. Converting to dex (d8) =="
find "$OBJ" -name '*.class' -print0 | xargs -0 "$BT/d8" --release --output "$DEXOUT" --lib "$PLATFORM"

echo "== 5. Packing classes.dex into the APK =="
cp -f "$BASEAPK" "$ALIGNEDAPK"
( cd "$DEXOUT" && jar uf "$ALIGNEDAPK" classes.dex )

echo "== 6. Aligning the APK (zipalign) =="
"$BT/zipalign" -f -p 4 "$ALIGNEDAPK" "$BUILD/aligned_final.apk"

echo "== 7. Signing key =="
if [ ! -f "$KEYSTORE" ]; then
    # Never a NEW key: an APK signed with one looks fine, and NO
    # already-installed phone can accept it as an update -- silently, and with
    # no way back once published. Minting a signing key can never be a build's
    # default behaviour: if it is missing, stop.
    cat >&2 <<EOF
Signing key not found:
  $KEYSTORE

This is the RELEASE key. Without it NO update can be published that phones
which already have the app will accept, and generating a new one is NOT a
fix: it breaks all of them for good.

If you moved it, point to it with the MIRRYL_KEYSTORE variable.
If it does not exist yet, create it ONCE with
create-keystore.ps1 (on Windows).
If it is really lost, this is an incident, not a build hiccup.
EOF
    exit 1
fi
echo "   $KEYSTORE (alias $KEYALIAS)"

echo "== 8. Signing the APK (apksigner) =="
# Password from the environment, never on this line. One variable for the
# keystore and the key, which share it; prompted without echo if unset.
if [ -z "${MIRRYL_KEYSTORE_PASS:-}" ]; then
  read -r -s -p "Keystore password ($KEYSTORE): " MIRRYL_KEYSTORE_PASS; echo
  export MIRRYL_KEYSTORE_PASS
fi
[ -n "${MIRRYL_KEYSTORE_PASS:-}" ] || { echo "No keystore password (MIRRYL_KEYSTORE_PASS)"; exit 1; }
"$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass env:MIRRYL_KEYSTORE_PASS --ks-key-alias "$KEYALIAS" \
    --key-pass env:MIRRYL_KEYSTORE_PASS --out "$SIGNEDAPK" "$BUILD/aligned_final.apk"

echo "== 9. APK version, for the PC app =="
# From the signed APK, not the manifest: what is written is what ships.
VERSIONCODE="$("$BT/aapt2" dump badging "$SIGNEDAPK" | sed -n "s/^package: name='com.mirryl.app' versionCode='\([0-9]*\)'.*/\1/p")"
[ -n "$VERSIONCODE" ] || { echo "Could not read the versionCode of $SIGNEDAPK with aapt2 dump badging"; exit 1; }
printf '%s\n' "$VERSIONCODE" > "$VERSIONFILE"
echo "   versionCode $VERSIONCODE -> $VERSIONFILE"

echo ""
echo "== DONE =="
echo "APK written to: $SIGNEDAPK"
