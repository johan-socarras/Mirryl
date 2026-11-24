#!/usr/bin/env bash
# Installs Mirryl on Linux (Debian, Ubuntu and their derivatives, x86-64) from
# the latest release:
#
#   curl -fsSL https://raw.githubusercontent.com/johan-socarras/Mirryl/main/install.sh | bash
#
# It downloads Mirryl_<version>_amd64.deb from GitHub, checks it against the
# SHA-256 GitHub publishes for it, and installs it with apt, which also
# brings in what Mirryl needs (adb, zenity, GStreamer and WebKitGTK). It asks
# for your password through sudo unless it runs as root. Running it again
# updates Mirryl in place. Optional:
#
#   MIRRYL_VERSION=v1.0.12   install that release instead of the latest
#   GITHUB_TOKEN=...         for a private fork, or to avoid rate limits
#
# (pass them to bash: curl -fsSL ... | MIRRYL_VERSION=v1.0.12 bash)

set -euo pipefail

main() {
    local repo="johan-socarras/Mirryl"

    fail() {
        printf '\033[31m%s\033[0m\nMirryl was not installed.\n' "$1" >&2
        exit 1
    }

    [ "$(uname -s)" = "Linux" ] || fail "This installer is for Linux. On Windows, use install.ps1: see the README."
    [ "$(uname -m)" = "x86_64" ] || fail "Mirryl for Linux is built for x86-64 only; this machine is $(uname -m)."
    command -v apt-get >/dev/null 2>&1 || fail "This installer needs apt (Debian, Ubuntu and their derivatives)."
    command -v curl >/dev/null 2>&1 || fail "This installer needs curl."

    local sudo=""
    if [ "$(id -u)" -ne 0 ]; then
        command -v sudo >/dev/null 2>&1 || fail "Run this as root, or install sudo."
        sudo="sudo"
    fi

    local auth=()
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
    fi
    local which="latest"
    if [ -n "${MIRRYL_VERSION:-}" ]; then
        which="tags/$MIRRYL_VERSION"
    fi

    local release
    release=$(curl -fsSL -H "Accept: application/vnd.github+json" -H "User-Agent: mirryl-install" \
        "${auth[@]}" "https://api.github.com/repos/$repo/releases/$which") \
        || fail "Could not find the release '$which' of $repo on GitHub."

    # Each asset of the release lists its fields in the same order, so the
    # n-th size, digest and URLs all belong to the n-th asset.
    local sizes digests urls apis
    sizes=$(printf '%s' "$release" | grep -oE '"size": *[0-9]+' | grep -oE '[0-9]+$' || true)
    digests=$(printf '%s' "$release" | grep -oE '"digest": *(null|"[^"]*")' | sed -E 's/.*: *"?([^"]*)"?$/\1/' || true)
    urls=$(printf '%s' "$release" | grep -oE '"browser_download_url": *"[^"]*"' | sed -E 's/.*"([^"]*)"$/\1/' || true)
    apis=$(printf '%s' "$release" | grep -oE '"url": *"https://api\.github\.com/repos/[^"]*/releases/assets/[0-9]+"' | sed -E 's/.*"([^"]*)"$/\1/' || true)
    local tag
    tag=$(printf '%s' "$release" | grep -oE '"tag_name": *"[^"]*"' | head -n 1 | sed -E 's/.*"([^"]*)"$/\1/')

    local index
    index=$(printf '%s\n' "$urls" | grep -nE '/Mirryl_[^/]+_amd64\.deb$' | head -n 1 | cut -d: -f1 || true)
    [ -n "$index" ] || fail "The release $tag has no Linux package."
    local url size digest api name
    url=$(printf '%s\n' "$urls" | sed -n "${index}p")
    size=$(printf '%s\n' "$sizes" | sed -n "${index}p")
    digest=$(printf '%s\n' "$digests" | sed -n "${index}p")
    api=$(printf '%s\n' "$apis" | sed -n "${index}p")
    name=${url##*/}

    local folder
    folder=$(mktemp -d)
    trap 'rm -rf "$folder"' EXIT
    chmod 755 "$folder"   # apt reads the package as its own _apt user
    local deb="$folder/$name"

    echo "Downloading $name ($(( size / 1048576 )) MB)..."
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        # A private repository only serves its files through the API.
        curl -fsSL -H "Accept: application/octet-stream" -H "User-Agent: mirryl-install" \
            "${auth[@]}" -o "$deb" "$api" || fail "The download failed."
    else
        curl -fsSL -H "User-Agent: mirryl-install" -o "$deb" "$url" || fail "The download failed."
    fi
    chmod 644 "$deb"

    local got
    got=$(stat -c %s "$deb")
    [ "$got" = "$size" ] || fail "The download is incomplete: $got bytes of $size."
    if [[ "$digest" =~ ^sha256:([0-9a-f]{64})$ ]]; then
        [ "$(sha256sum "$deb" | cut -d' ' -f1)" = "${BASH_REMATCH[1]}" ] \
            || fail "The download does not match the SHA-256 GitHub publishes for it."
        echo "Checked against the SHA-256 published on GitHub."
    else
        echo "GitHub publishes no SHA-256 for this file; checked its size only."
    fi

    echo "Installing Mirryl $tag..."
    $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --allow-downgrades "$deb" \
        || fail "apt could not install the package."

    local version
    version=$(dpkg-query -W -f='${Version}' mirryl 2>/dev/null || true)
    [ -n "$version" ] || fail "apt finished, but the mirryl package is not installed."
    printf '\033[32mMirryl %s is installed.\033[0m\n' "$version"
    echo "Open it from your applications menu, or run Mirryl in a terminal."
}

main "$@"
