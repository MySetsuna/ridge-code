#!/usr/bin/env bash
set -euo pipefail

readonly MIN_LINE_COVERAGE=80
readonly QUALITY_DIR="target/quality"
readonly LCOV_PATH="${QUALITY_DIR}/lcov.info"

mkdir -p "${QUALITY_DIR}"
cargo fmt --all -- --check
git diff --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
mkdir -p "${QUALITY_DIR}"
cargo clippy --workspace --all-targets --locked --message-format=json -- -D warnings > "${QUALITY_DIR}/clippy.json"
cargo build --workspace --locked

if ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "cargo-llvm-cov is required; install it before running the quality gate" >&2
  exit 1
fi
cargo llvm-cov --workspace --all-features --locked --lcov \
  --output-path "${LCOV_PATH}" --fail-under-lines "${MIN_LINE_COVERAGE}"
test -s "${LCOV_PATH}"
test -s "${QUALITY_DIR}/clippy.json"

SONAR_SCANNER_BIN="$(command -v sonar-scanner || command -v sonar-scanner-npm || true)"
test -n "${SONAR_SCANNER_BIN}" || {
  echo "sonar-scanner or sonar-scanner-npm is required; the quality gate cannot skip Sonar" >&2
  exit 1
}
test -n "${SONAR_TOKEN:-}" || {
  echo "SONAR_TOKEN is required; the quality gate cannot run unauthenticated" >&2
  exit 1
}
if [[ "${SONAR_HOST_URL:-http://localhost:9000}" =~ ^https?://(localhost|127\.0\.0\.1)(:|/|$) ]]; then
  export NO_PROXY="${NO_PROXY:+${NO_PROXY},}localhost,127.0.0.1"
  export no_proxy="${NO_PROXY}"
fi
"${SONAR_SCANNER_BIN}" \
  "-Dsonar.host.url=${SONAR_HOST_URL:-http://localhost:9000}" \
  -Dsonar.qualitygate.wait=true \
  -Dsonar.qualitygate.timeout=300
echo "Quality gate passed: line coverage >= ${MIN_LINE_COVERAGE}% and Sonar quality gate passed."
