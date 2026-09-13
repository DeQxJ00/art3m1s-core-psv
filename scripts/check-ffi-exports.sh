#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

required=("art3m1s_get_api_v1")
allowed=(
  "art3m1s_get_api_v1"
  "art3m1s_rfvp_get_api_v1"
  "art3m1s_krkr_get_api_v1"
)
library=""

while (($#)); do
  case "$1" in
    --require-rfvp)
      required+=("art3m1s_rfvp_get_api_v1")
      ;;
    --require-krkr)
      required+=("art3m1s_krkr_get_api_v1")
      ;;
    -h|--help)
      cat <<'EOF'
Usage: check-ffi-exports.sh [--require-rfvp] [--require-krkr] [library]

The core facade is always required. Engine facades are allowed when their
feature is compiled and can be made mandatory for a specific build.
EOF
      exit 0
      ;;
    -*)
      echo "Unknown option: $1" >&2
      exit 2
      ;;
    *)
      if [[ -n "$library" ]]; then
        echo "Only one library path may be specified." >&2
        exit 2
      fi
      library="$1"
      ;;
  esac
  shift
done

case "$(uname -s)" in
  Darwin)
    default_lib="target/release/libart3m1s_core.dylib"
    symbols() { nm -gU "$1"; }
    ;;
  Linux)
    default_lib="target/release/libart3m1s_core.so"
    symbols() { nm -D --defined-only "$1"; }
    ;;
  *)
    echo "Unsupported platform for FFI export check: $(uname -s)" >&2
    exit 1
    ;;
esac

library="${library:-$default_lib}"
if [[ ! -f "$library" ]]; then
  echo "Missing $library; run cargo build --release --lib first." >&2
  exit 1
fi

actual="$(
  symbols "$library" |
    awk '{print $NF}' |
    sed 's/^_//' |
    grep '^art3m1s_' |
    sort -u || true
)"

missing="$(
  for symbol in "${required[@]}"; do
    if ! grep -qxF "$symbol" <<<"$actual"; then
      printf '%s\n' "$symbol"
    fi
  done
)"

unexpected="$(
  while IFS= read -r symbol; do
    if [[ -z "$symbol" ]]; then
      continue
    fi
    if ! printf '%s\n' "${allowed[@]}" | grep -qxF "$symbol"; then
      printf '%s\n' "$symbol"
    fi
  done <<<"$actual"
)"

if [[ -n "$missing" || -n "$unexpected" ]]; then
  printf 'Unexpected art3m1s FFI exports in %s.\n' "$library" >&2
  if [[ -n "$missing" ]]; then
    printf 'Missing required exports:\n%s\n' "$missing" >&2
  fi
  if [[ -n "$unexpected" ]]; then
    printf 'Exports outside the allowlist:\n%s\n' "$unexpected" >&2
  fi
  printf 'Allowed exports:\n%s\nActual exports:\n%s\n' \
    "$(printf '%s\n' "${allowed[@]}")" "$actual" >&2
  exit 1
fi

printf 'FFI export allowlist OK: %s (required: %s)\n' \
  "$library" "${required[*]}"
