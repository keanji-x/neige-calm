#!/usr/bin/env bash
# Classify a NUL-delimited git path stream conservatively for CI fan-out.
#
# Each mode names an independently executable CI surface. A path returns false
# only when it belongs to a known-unrelated tree; unknown paths, an empty diff,
# and unavailable comparison SHAs fail open to the broadest safe coverage.
# `docs/oracle/**` is documentation for ordinary code jobs but executable input
# to the mutation catalog, so mutation mode deliberately treats it as relevant.
set -euo pipefail

mode="${1:-code}"
case "$mode" in
  code|rust|fe|web|openapi|fe-e2e|stack|mutation) ;;
  *) echo "unknown change-classification mode: $mode" >&2; exit 2 ;;
esac

is_documentation_path() {
  case "$1" in
    docs/*|README.md|*/README.md|CHANGELOG.md|CONTRIBUTING.md|SECURITY.md|CODE_OF_CONDUCT.md|LICENSE|LICENSE.*|NOTICE|NOTICE.*) return 0 ;;
    *) return 1 ;;
  esac
}

is_ci_authority_path() {
  case "$1" in
    .github/workflows/*|scripts/ci/classify-code-changes.sh|scripts/ci/classify-code-changes-selftest.sh) return 0 ;;
    *) return 1 ;;
  esac
}

seen=false
relevant=false
while IFS= read -r -d '' path; do
  seen=true
  if is_ci_authority_path "$path"; then
    relevant=true
    continue
  fi

  case "$mode" in
    code)
      if ! is_documentation_path "$path"; then
        relevant=true
      fi
      ;;
    rust)
      if is_documentation_path "$path"; then
        continue
      fi
      case "$path" in
        fe/*|web/*) ;;
        *.rs|Cargo.toml|Cargo.lock|*/Cargo.toml|rust-toolchain|rust-toolchain.toml|rustfmt.toml|.cargo/*|.config/nextest.toml|crates/*|plugins/*|scripts/*|e2e/*|docker/*|Makefile|docker-compose.yml) relevant=true ;;
        *) relevant=true ;;
      esac
      ;;
    fe)
      if is_documentation_path "$path"; then
        continue
      fi
      case "$path" in
        fe/*) relevant=true ;;
        crates/*|plugins/*|web/*|scripts/*|e2e/*|docker/*|Cargo.toml|Cargo.lock|rust-toolchain|rust-toolchain.toml|rustfmt.toml|.cargo/*|.config/*|Makefile|docker-compose.yml) ;;
        *) relevant=true ;;
      esac
      ;;
    web)
      if is_documentation_path "$path"; then
        continue
      fi
      case "$path" in
        web/*|fe/core/keys/storage.ts|fe/core/api/generated/wire.ts) relevant=true ;;
        crates/*|plugins/*|fe/*|scripts/*|e2e/*|docker/*|Cargo.toml|Cargo.lock|rust-toolchain|rust-toolchain.toml|rustfmt.toml|.cargo/*|.config/*|Makefile|docker-compose.yml) ;;
        *) relevant=true ;;
      esac
      ;;
    openapi)
      if is_documentation_path "$path"; then
        continue
      fi
      case "$path" in
        Cargo.toml|Cargo.lock|*/Cargo.toml|rust-toolchain|rust-toolchain.toml|.cargo/*|crates/*/src/*|web/package.json|web/package-lock.json|web/src/api/openapi.json|web/src/api/generated.ts|web/src/api/generated-terminal.ts|web/src/api/generated-events.ts|web/src/editor/types/*|fe/core/api/generated/*) relevant=true ;;
        crates/*|plugins/*|fe/*|web/*|scripts/*|e2e/*|docker/*|rustfmt.toml|.config/*|Makefile|docker-compose.yml) ;;
        *) relevant=true ;;
      esac
      ;;
    fe-e2e)
      if is_documentation_path "$path"; then
        continue
      fi
      case "$path" in
        crates/calm-server/tests/fixtures/osc-probe-child/*) relevant=true ;;
        crates/*/tests/*|crates/*/benches/*) ;;
        crates/*/src/*|crates/*/migrations/*|plugins/*|Cargo.toml|Cargo.lock|*/Cargo.toml|rust-toolchain|rust-toolchain.toml|docker/*|docker-compose.yml|Makefile) relevant=true ;;
        fe/e2e/*|fe/package.json|fe/package-lock.json|fe/vite.config.*|fe/web/index.html) relevant=true ;;
        fe/*.test.*|fe/*.spec.*|fe/*/tests/*|fe/*/__tests__/*|fe/tools/*) ;;
        fe/*) relevant=true ;;
        web/*|scripts/*|e2e/*|rustfmt.toml|.cargo/*|.config/*) ;;
        *) relevant=true ;;
      esac
      ;;
    stack)
      if is_documentation_path "$path"; then
        continue
      fi
      case "$path" in
        crates/*/tests/*|crates/*/benches/*) ;;
        crates/*/src/*|crates/*/migrations/*|plugins/*|Cargo.toml|Cargo.lock|*/Cargo.toml|rust-toolchain|rust-toolchain.toml) relevant=true ;;
        docker/*|docker-compose.yml|Makefile|e2e/*) relevant=true ;;
        fe/package.json|fe/package-lock.json|fe/vite.config.*|fe/web/index.html|web/package.json|web/package-lock.json|web/vite.config.*|web/index.html) relevant=true ;;
        fe/*|web/*|scripts/*|rustfmt.toml|.cargo/*|.config/*) ;;
        *) relevant=true ;;
      esac
      ;;
    mutation)
      case "$path" in
        docs/oracle/*) relevant=true ;;
        *)
          if is_documentation_path "$path"; then
            continue
          fi
          case "$path" in
            fe/*) relevant=true ;;
            crates/*|plugins/*|web/*|scripts/*|e2e/*|docker/*|Cargo.toml|Cargo.lock|rust-toolchain|rust-toolchain.toml|rustfmt.toml|.cargo/*|.config/*|Makefile|docker-compose.yml) ;;
            *) relevant=true ;;
          esac
          ;;
      esac
      ;;
  esac
done

if [ "$seen" = false ]; then
  relevant=true
fi

printf '%s\n' "$relevant"
