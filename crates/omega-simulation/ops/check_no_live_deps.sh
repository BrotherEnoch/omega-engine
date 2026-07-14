# omega-engine\crates\omega-simulation\ops\check_no_live_deps.sh
#!/usr/bin/env bash
# Fails CI if omega-simulation's dependency tree contains anything that
# looks like a live relay client or a real signing/HSM backend.
#
# This is a belt-and-suspenders check on top of the crate-level design
# (SimulationSubmitter can only be constructed from a ForkHandle, and
# reject_if_live_looking() screens destination strings at runtime). Dependency
# graphs can grow accidental transitive edges over time; this catches that.
#
# Usage: run from the omega-simulation crate directory.
#   ./ops/check_no_live_deps.sh

set -euo pipefail

FORBIDDEN_PATTERNS=(
  "flashbots"
  "bloxroute"
  "titan-relay"
  "eden-network"
  "mev-share"
  "aws-sdk-kms"
  "hsm"
)

echo "Checking omega-simulation dependency tree for forbidden live-transport crates..."

TREE_OUTPUT="$(cargo tree --package omega-simulation 2>/dev/null || cargo tree)"

FAILED=0
for pattern in "${FORBIDDEN_PATTERNS[@]}"; do
  if echo "$TREE_OUTPUT" | grep -qi "$pattern"; then
    echo "FAIL: forbidden dependency pattern '$pattern' found in omega-simulation's dependency tree."
    FAILED=1
  fi
done

if [ "$FAILED" -ne 0 ]; then
  echo ""
  echo "omega-simulation must never depend on live relay or signing infrastructure."
  echo "That code belongs in omega-execution. Remove the offending dependency."
  exit 1
fi

echo "OK: no forbidden live-transport dependencies found."