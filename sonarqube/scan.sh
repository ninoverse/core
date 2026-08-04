#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
CRATE_NAME="ninoverse"
CLIPPY_REPORT="$REPO_ROOT/clippy-report.json"
LCOV_REPORT="$REPO_ROOT/lcov.info"

# --- token: env first, then the gitignored file. never hardcoded. ----------
TOKEN_FILE="${SONAR_TOKEN_FILE:-$SCRIPT_DIR/.sonar-token}"
if [[ -z "${SONAR_TOKEN:-}" && -f "$TOKEN_FILE" ]]; then
  SONAR_TOKEN="$(tr -d '[:space:]' < "$TOKEN_FILE")"
fi
: "${SONAR_TOKEN:?SONAR_TOKEN is unset and $TOKEN_FILE does not exist}"
export SONAR_TOKEN

# --- branch name for the community branch plugin --------------------------
BRANCH="$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD)"
if [[ "$BRANCH" == "HEAD" ]]; then                 # detached HEAD
  BRANCH="detached-$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
fi
echo ">> analyzing branch $BRANCH"

cd "$REPO_ROOT"

# --- clippy on the HOST: reuses the warm target/ cache, and the scanner ---
# --- container cannot write to the read-only mount anyway.             ----
echo ">> generating clippy report"
# clippy caches diagnostics in target/, so a second run emits nothing.
# Invalidate only the local crate (cheap) to force a re-lint every time.
cargo clean -p "$CRATE_NAME"
set +e
cargo clippy --all-targets --message-format=json > "$CLIPPY_REPORT"
CLIPPY_STATUS=$?
set -e
if [[ ! -s "$CLIPPY_REPORT" ]]; then
  echo "!! clippy produced no output (exit $CLIPPY_STATUS) - aborting" >&2
  exit 1
fi

# --- coverage ------------------------------------------------------------
echo ">> generating coverage report"
# --remap-path-prefix writes SF: paths relative to the workspace root.
# Without it lcov records absolute host paths, which cannot resolve against
# sonar.projectBaseDir=/usr/src in the container, and coverage imports as 0.
cargo llvm-cov --workspace --lcov --remap-path-prefix --output-path "$LCOV_REPORT"

# --- scan the whole repository -------------------------------------------
docker build -t custom-sonar-scanner-rust "$SCRIPT_DIR"

docker run --rm \
  -e SONAR_HOST_URL="http://sonarqube:9000/sonarqube" \
  -e SONAR_TOKEN \
  -v "$REPO_ROOT:/usr/src:ro" \
  --tmpfs /usr/src/target \
  --network ninoverse-network \
  custom-sonar-scanner-rust \
  sonar-scanner -Dsonar.branch.name="$BRANCH"
