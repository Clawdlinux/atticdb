#!/usr/bin/env bash
# Publishes the 4 AtticDB crates to crates.io at a release version, in dependency order.
#
#   scripts/crates-publish.sh 0.4.0            # publish
#   scripts/crates-publish.sh 0.4.0 --dry-run  # package and build every crate, upload nothing
#
# Main keeps Cargo.toml at its placeholder version, so this edits the working copy only: it sets
# the workspace version and pins each path dependency to it. Run it in a CI checkout, never on
# a tree you mean to commit. A crate whose version is already on crates.io is skipped, so a
# rerun after a partial publish finishes the rest. Publishing needs CARGO_REGISTRY_TOKEN.

set -euo pipefail

version=${1:?usage: crates-publish.sh <x.y.z> [--dry-run]}
mode=${2:-}
if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "crates-publish: version must be x.y.z, got $version" >&2
    exit 2
fi
if [ -n "$mode" ] && [ "$mode" != --dry-run ]; then
    echo "crates-publish: unknown flag $mode" >&2
    exit 2
fi

cd "$(dirname "$0")/.."
crates=(atticdb-sys atticdb-tape atticdb-core atticdb)

# The workspace version, then a version beside every `path = "../atticdb-*"` dependency.
# A crate's dev-dependency on itself (`path = "."`) stays path-only, so cargo drops it.
sed -i.bak "/^\[workspace\.package\]/,/^\[/s/^version = \".*\"$/version = \"$version\"/" Cargo.toml
for crate in "${crates[@]}"; do
    sed -i.bak -E "s#(\{ path = \"\.\./atticdb-[a-z]+\")#\1, version = \"=$version\"#" \
        "crates/$crate/Cargo.toml"
done
find . -name Cargo.toml.bak -not -path './target/*' -delete
# Only the workspace crates' own entries change in Cargo.lock.
cargo update --workspace

if [ "$mode" = --dry-run ]; then
    # Packages every crate against the others' local packages, then builds each one.
    cargo publish --workspace --locked --allow-dirty --dry-run
    exit 0
fi

: "${CARGO_REGISTRY_TOKEN:?crates-publish: CARGO_REGISTRY_TOKEN is not set}"
for crate in "${crates[@]}"; do
    status=$(curl -s -o /dev/null -w '%{http_code}' -A 'atticdb-release (github.com/Clawdlinux/atticdb)' \
        "https://crates.io/api/v1/crates/$crate/$version")
    case "$status" in
        200)
            echo "$crate $version is already on crates.io, skipping"
            continue
            ;;
        404) ;;
        *)
            echo "crates-publish: crates.io answered $status for $crate $version" >&2
            exit 1
            ;;
    esac
    # cargo waits until the crate is in the index, so the next crate can depend on it.
    cargo publish -p "$crate" --locked --allow-dirty
done
