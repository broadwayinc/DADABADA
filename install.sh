#!/bin/sh
# DDBD installer. Served at https://cdn.broadwayinc.com/get/ddbd/v1 and run with:
#   curl -fsSL https://cdn.broadwayinc.com/get/ddbd/v1 | sh
#
# It downloads the DDBD build for this machine, checks its SHA-256, and installs the
# `ddbd` program into /usr/local/bin (with sudo when needed), or into ~/.local/bin when
# that is not possible. The license files go to <prefix>/share/doc/ddbd.
#
# Optional settings, given as environment variables to `sh`:
#   DDBD_VERSION=0.2.0           install this version instead of the latest one
#   DDBD_INSTALL_DIR=/some/bin   install the program into this folder
#   DDBD_BASE_URL=https://...    download from another place (testing a release)
#
# Everything runs inside main(), so nothing happens if the download of this script stops halfway.

set -eu

say() { printf '%s\n' "$*"; }
fail() { printf 'DDBD install failed: %s\n' "$*" >&2; exit 1; }

main() {
    base="${DDBD_BASE_URL:-https://cdn.broadwayinc.com/get/ddbd}"

    # --- this machine ---
    os="$(uname -s)"
    [ "$os" = "Darwin" ] && fail "this script installs on Linux. On macOS, install DDBD with Homebrew: brew install broadwayinc/ddbd/ddbd"
    [ "$os" = "Linux" ] || fail "DDBD runs on Linux and macOS; this machine runs $os."
    case "$(uname -m)" in
        x86_64 | amd64) arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) fail "there is no DDBD build for the processor type $(uname -m)." ;;
    esac
    glibc="$(ldd --version 2>/dev/null | head -n 1 | awk '{print $NF}')"
    case "$glibc" in
        [0-9]*.[0-9]*) ;;
        *) fail "DDBD needs a Linux with glibc 2.34 or newer (for example Ubuntu 22.04, Debian 12, RHEL 9, Amazon Linux 2023). This system's C library could not be read as glibc (Alpine and other musl systems are not supported)." ;;
    esac
    major="${glibc%%.*}"
    minor="${glibc#*.}"
    minor="${minor%%[!0-9]*}"
    if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 34 ]; }; then
        fail "DDBD needs glibc 2.34 or newer (for example Ubuntu 22.04, Debian 12, RHEL 9, Amazon Linux 2023); this system has glibc $glibc."
    fi

    # --- download tools ---
    if command -v curl >/dev/null 2>&1; then
        fetch() { curl -fsSL "$1" -o "$2"; }
    elif command -v wget >/dev/null 2>&1; then
        fetch() { wget -q "$1" -O "$2"; }
    else
        fail "curl or wget is needed to download DDBD."
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        sha256() { sha256sum "$1" | awk '{print $1}'; }
    elif command -v shasum >/dev/null 2>&1; then
        sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
    else
        fail "sha256sum (or shasum) is needed to check the download."
    fi

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    # --- which version ---
    version="${DDBD_VERSION:-}"
    if [ -z "$version" ]; then
        fetch "$base/latest" "$tmp/latest" || fail "could not read the latest version from $base/latest."
        version="$(tr -d ' \r\n' < "$tmp/latest")"
    fi
    case "$version" in
        [0-9]*.[0-9]*.[0-9]*) ;;
        *) fail "'$version' is not a DDBD version." ;;
    esac

    # --- download and check ---
    name="ddbd-$version-linux-$arch"
    say "Downloading DDBD $version for Linux $arch..."
    fetch "$base/$version/$name.tar.gz" "$tmp/$name.tar.gz" || fail "could not download $base/$version/$name.tar.gz."
    fetch "$base/$version/$name.tar.gz.sha256" "$tmp/$name.tar.gz.sha256" || fail "could not download the checksum $base/$version/$name.tar.gz.sha256."
    want="$(awk '{print $1}' < "$tmp/$name.tar.gz.sha256")"
    got="$(sha256 "$tmp/$name.tar.gz")"
    [ "$want" = "$got" ] || fail "the download is damaged: its SHA-256 is $got, expected $want. Run the command again."
    tar -xzf "$tmp/$name.tar.gz" -C "$tmp" || fail "could not unpack $name.tar.gz."
    [ -f "$tmp/$name/ddbd" ] || fail "the download does not contain the ddbd program."

    # --- where to install ---
    sudo=""
    if [ -n "${DDBD_INSTALL_DIR:-}" ]; then
        bin="$DDBD_INSTALL_DIR"
        docs="$(dirname "$bin")/share/doc/ddbd"
    else
        bin="/usr/local/bin"
        docs="/usr/local/share/doc/ddbd"
        if [ "$(id -u)" -ne 0 ] && ! { [ -d "$bin" ] && [ -w "$bin" ]; }; then
            if command -v sudo >/dev/null 2>&1; then
                sudo="sudo"
                say "Installing into $bin needs administrator rights; sudo may ask for your password."
            else
                bin="$HOME/.local/bin"
                docs="$HOME/.local/share/doc/ddbd"
            fi
        fi
    fi
    if [ -n "$sudo" ]; then
        # read the password from the terminal, even though this script itself arrives through a pipe
        if [ -r /dev/tty ]; then $sudo -v < /dev/tty || fail "sudo did not accept the password."; else $sudo -v || fail "sudo is needed to write to $bin."; fi
    fi

    $sudo mkdir -p "$bin" "$docs"
    $sudo install -m 0755 "$tmp/$name/ddbd" "$bin/ddbd"
    for f in LICENSE NOTICE THIRD-PARTY-NOTICES; do
        [ -f "$tmp/$name/$f" ] && $sudo install -m 0644 "$tmp/$name/$f" "$docs/$f"
    done

    installed="$("$bin/ddbd" version 2>/dev/null)" || fail "$bin/ddbd was installed but does not run on this system."
    say ""
    say "Installed $installed at $bin/ddbd."
    say "License files: $docs"
    case ":$PATH:" in
        *":$bin:"*) ;;
        *) say ""
           say "$bin is not on your PATH yet. Add this line to ~/.profile, then open a new terminal:"
           say "  export PATH=\"$bin:\$PATH\"" ;;
    esac
    say ""
    say "Next, set up a database folder and start the server:"
    say "  ddbd init --data ~/ddbd-data"
    say "  ddbd start --data ~/ddbd-data"
    say "Running this command again later updates DDBD to the newest version."
}

main "$@"
