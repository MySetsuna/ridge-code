#!/usr/bin/env bash
set -u

ok=true
checks=()
check_command() {
  local name="$1"; shift
  if command -v "$1" >/dev/null 2>&1 && "$@" >/dev/null 2>&1; then
    checks+=("{\"name\":\"$name\",\"passed\":true,\"detail\":\"available\"}")
  else
    checks+=("{\"name\":\"$name\",\"passed\":false,\"detail\":\"missing or not executable\"}")
    ok=false
  fi
}

check_command cargo cargo --version
check_command npm npm --version
check_command cargo-llvm-cov cargo llvm-cov --version

scanner=""
if command -v sonar-scanner >/dev/null 2>&1; then scanner=sonar-scanner
elif command -v sonar-scanner-npm >/dev/null 2>&1; then scanner=sonar-scanner-npm
fi
if [[ -n "$scanner" ]] && "$scanner" --version >/dev/null 2>&1; then
  checks+=("{\"name\":\"sonar_scanner\",\"passed\":true,\"detail\":\"available\"}")
else
  checks+=("{\"name\":\"sonar_scanner\",\"passed\":false,\"detail\":\"sonar-scanner or sonar-scanner-npm missing\"}")
  ok=false
fi

if [[ -n "${SONAR_TOKEN:-}" ]]; then
  checks+=("{\"name\":\"sonar_token\",\"passed\":true,\"detail\":\"present (value redacted)\"}")
else
  checks+=("{\"name\":\"sonar_token\",\"passed\":false,\"detail\":\"SONAR_TOKEN is not set\"}")
  ok=false
fi

printf '{"schema_version":1,"kind":"ridgecode_quality_preflight","ready":%s,"checks":[%s]}\n' \
  "$ok" "$(IFS=,; echo "${checks[*]}")"
if [[ "$ok" != true ]]; then exit 1; fi
