#!/usr/bin/env bash
# Creates the 3 DNS-only records that point atticdb.com at Vercel.
# Usage: scripts/cf-dns.sh [token-file]   (default: ~/.config/atticdb/cf-dns-token)
# The token needs Zone DNS Edit on atticdb.com. It is read from the file and never printed.
# Existing records are never changed. A record that differs stops the script.
set -euo pipefail

ZONE_NAME="atticdb.com"
TOKEN_FILE="${1:-$HOME/.config/atticdb/cf-dns-token}"
API="https://api.cloudflare.com/client/v4"

[ -r "$TOKEN_FILE" ] || { echo "No token file at $TOKEN_FILE" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
chmod 700 "$work"
# Headers go through a 0600 file so the token never shows in a process argument.
umask 077
printf 'Authorization: Bearer %s\nContent-Type: application/json\n' "$(tr -d '[:space:]' < "$TOKEN_FILE")" > "$work/headers"

cf() { curl -sS --max-time 20 -H @"$work/headers" "$@"; }

zone_id=$(cf "$API/zones?name=$ZONE_NAME" | jq -r '.result[0].id // empty')
[ -n "$zone_id" ] || { echo "Zone $ZONE_NAME not found, or the token can't read it" >&2; exit 1; }

# name type content
records=(
  "atticdb.com A 76.76.21.21"
  "www.atticdb.com CNAME cname.vercel-dns.com"
  "docs.atticdb.com CNAME cname.vercel-dns.com"
)

for rec in "${records[@]}"; do
  read -r name type content <<<"$rec"
  existing=$(cf "$API/zones/$zone_id/dns_records?name=$name" | jq -c '.result')
  count=$(jq 'length' <<<"$existing")
  if [ "$count" -gt 0 ]; then
    same=$(jq --arg t "$type" --arg c "$content" '[.[] | select(.type==$t and .content==$c)] | length' <<<"$existing")
    if [ "$same" -gt 0 ]; then
      echo "ok      $name $type $content (already there)"
      continue
    fi
    echo "stop    $name already has other records:" >&2
    jq -r '.[] | "        \(.type) \(.content) proxied=\(.proxied)"' <<<"$existing" >&2
    echo "        Remove or change them in the dashboard, then run this again." >&2
    exit 1
  fi
  body=$(jq -nc --arg t "$type" --arg n "$name" --arg c "$content" '{type:$t,name:$n,content:$c,ttl:1,proxied:false}')
  ok=$(cf -X POST "$API/zones/$zone_id/dns_records" --data "$body" | jq -r '.success')
  [ "$ok" = "true" ] || { echo "failed  $name $type $content" >&2; exit 1; }
  echo "created $name $type $content (DNS only)"
done

echo "Done. Vercel issues certificates once these resolve. Check with: vercel domains inspect atticdb.com"
