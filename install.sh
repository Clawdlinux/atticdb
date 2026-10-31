#!/bin/sh
# Installs the atticdb binary from a GitHub release.
#
#   curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh
#
# Settings, all optional:
#   ATTICDB_VERSION        a release such as 0.1.0 or v0.1.0. Default: latest.
#   ATTICDB_INSTALL_DIR    where atticdb goes. Default: $HOME/.local/bin.
#   ATTICDB_DOWNLOAD_BASE  the releases URL. Default: https://github.com/Clawdlinux/atticdb/releases.
#
# The tarball is checked against the release's SHA256SUMS before anything is installed.
# It never uses sudo and never edits shell rc files.

set -eu

FROM_SOURCE='cargo install --git https://github.com/Clawdlinux/atticdb atticdb --locked'

say() {
    printf 'atticdb install: %s\n' "$*"
}

fail() {
    printf 'atticdb install: %s\n' "$*" >&2
    exit 1
}

# Prints the release target for this OS and CPU, or fails.
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Darwin/arm64 | Darwin/aarch64)
            echo aarch64-apple-darwin
            ;;
        Darwin/x86_64)
            # A shell under Rosetta reports x86_64 on Apple silicon. The native binary is better.
            if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                echo aarch64-apple-darwin
            else
                echo x86_64-apple-darwin
            fi
            ;;
        Linux/x86_64)
            echo x86_64-unknown-linux-musl
            ;;
        Linux/aarch64 | Linux/arm64)
            echo aarch64-unknown-linux-musl
            ;;
        MINGW* | MSYS* | CYGWIN*)
            # Windows gets a zip. This script installs nothing there.
            printf 'atticdb install: on Windows, download atticdb-x86_64-pc-windows-msvc.zip from\n' >&2
            printf '  https://github.com/Clawdlinux/atticdb/releases/latest\n' >&2
            printf 'Put atticdb.exe in a folder on your PATH, then run: atticdb setup\n' >&2
            exit 1
            ;;
        *)
            printf 'atticdb install: no prebuilt binary for %s %s.\n' "$os" "$arch" >&2
            printf 'Build it from source with Rust instead:\n  %s\n' "$FROM_SOURCE" >&2
            exit 1
            ;;
    esac
}

# download <url> <file>
download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1" || fail "download failed: $1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1" || fail "download failed: $1"
    else
        fail "needs curl or wget"
    fi
}

# Prints the SHA-256 of a file in lowercase hex.
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    else
        fail "needs sha256sum or shasum to check the download"
    fi
}

main() {
    target=$(detect_target)
    file="atticdb-$target.tar.gz"

    base=${ATTICDB_DOWNLOAD_BASE:-https://github.com/Clawdlinux/atticdb/releases}
    base=${base%/}
    version=${ATTICDB_VERSION:-latest}
    case "$version" in
        latest)
            url="$base/latest/download"
            ;;
        *)
            version=${version#v}
            case "$version" in
                '' | *[!0-9A-Za-z.+-]*) fail "ATTICDB_VERSION is not a version: $ATTICDB_VERSION" ;;
            esac
            url="$base/download/v$version"
            ;;
    esac

    if [ -n "${ATTICDB_INSTALL_DIR:-}" ]; then
        dir=$ATTICDB_INSTALL_DIR
    elif [ -n "${HOME:-}" ]; then
        dir="$HOME/.local/bin"
    else
        fail "HOME is not set. Set ATTICDB_INSTALL_DIR to an absolute path."
    fi
    case "$dir" in
        /) ;;
        /*) dir=${dir%/} ;;
        *) fail "ATTICDB_INSTALL_DIR must be an absolute path: $dir" ;;
    esac

    tmp=$(mktemp -d "${TMPDIR:-/tmp}/atticdb-install.XXXXXX")
    part=''
    trap 'rm -rf "$tmp"; if [ -n "$part" ]; then rm -f "$part"; fi' EXIT
    trap 'exit 1' HUP INT TERM

    say "downloading $url/$file"
    download "$url/$file" "$tmp/$file"
    download "$url/SHA256SUMS" "$tmp/SHA256SUMS"

    want=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1; exit }' "$tmp/SHA256SUMS")
    [ -n "$want" ] || fail "SHA256SUMS has no entry for $file. Nothing was installed."
    got=$(sha256 "$tmp/$file")
    if [ "$got" != "$want" ]; then
        fail "checksum mismatch for $file: expected $want, got $got. Nothing was installed."
    fi

    mkdir "$tmp/x"
    tar -xzf "$tmp/$file" -C "$tmp/x" || fail "cannot extract $file"
    [ -f "$tmp/x/atticdb" ] || fail "$file has no atticdb binary"

    # Copy beside the target, then rename over it, so the swap is atomic. The copy runs once
    # first, from the install dir, since TMPDIR may be mounted noexec. On failure the trap
    # removes it.
    mkdir -p "$dir"
    dest="$dir/atticdb"
    # mktemp makes a new 0600 file, so a link planted at a guessable name can't redirect cp.
    part=$(mktemp "$dir/.atticdb.part.XXXXXX") || fail "cannot create a temp file in $dir"
    cp "$tmp/x/atticdb" "$part"
    chmod 755 "$part"
    "$part" --version >/dev/null 2>&1 ||
        fail "the downloaded atticdb did not run from $dir. Nothing was installed."
    # mv would put the file inside a directory, or a symlink to one, and report success.
    if [ -d "$dest" ]; then
        fail "$dest is a directory. Move it away and run the installer again. Nothing was installed."
    fi
    mv -f "$part" "$dest"
    part=''

    installed=$("$dest" --version) || fail "$dest --version failed after the install."
    say "installed $installed at $dest"

    found=$(command -v atticdb 2>/dev/null || true)
    case ":${PATH:-}:" in
        *":$dir:"* | *":$dir/:"*)
            # dash prints $dir//atticdb for a PATH entry with a trailing slash.
            if [ -n "$found" ] && [ "$found" != "$dest" ] && [ "$found" != "$dir//atticdb" ]; then
                say "warning: $found comes first on PATH, so atticdb runs that one."
                say "Remove it, or put $dir earlier on PATH."
            fi
            ;;
        *)
            say "warning: $dir is not on PATH. Add it in your shell's rc file:"
            # Single quotes keep $, `, " and \ in the dir literal. Each ' becomes '\''.
            quoted=$(printf '%s' "$dir" | sed "s/'/'\\\\''/g")
            printf "  export PATH='%s':\"\$PATH\"\n" "$quoted"
            if [ -n "$found" ]; then
                say "warning: atticdb on PATH is $found, not the one just installed."
            fi
            ;;
    esac

    printf '\nNext steps:\n'
    printf '  atticdb setup --dry-run   # shows what it would change, writes nothing\n'
    printf '  atticdb setup             # asks which apps, then wires them\n'
}

main "$@"
