#!/usr/bin/env bash
# Deploys the docs project (docs.atticdb.com) to Vercel.
# Vercel's remote build always reads vercel.json, so this stages a copy of site/
# where vercel.docs.json becomes vercel.json, then deploys that copy.
# Usage: scripts/deploy-docs.sh [--prod]
set -euo pipefail

SITE="$(cd "$(dirname "$0")/.." && pwd)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

ORG_ID="${VERCEL_ORG_ID:-$(jq -r .orgId "$SITE/.vercel/project.json")}"
PROJECT_ID="${VERCEL_DOCS_PROJECT_ID:-$(vercel project inspect atticdb-docs 2>&1 | grep -oE 'prj_[A-Za-z0-9]+' | head -1 || true)}"
[ -n "$PROJECT_ID" ] || { echo "Can't find the atticdb-docs project id" >&2; exit 1; }

rsync -a --exclude-from="$SITE/.vercelignore" "$SITE/" "$STAGE/"
mv "$STAGE/vercel.docs.json" "$STAGE/vercel.json"
mkdir -p "$STAGE/.vercel"
printf '{"projectId":"%s","orgId":"%s"}\n' "$PROJECT_ID" "$ORG_ID" > "$STAGE/.vercel/project.json"

cd "$STAGE"
vercel deploy --yes "$@"
