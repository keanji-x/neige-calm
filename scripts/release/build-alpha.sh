#!/usr/bin/env bash
# Build the Linux Alpha artifact from this exact, clean checkout.
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: scripts/release/build-alpha.sh --version 0.1.0-alpha.1 --output-dir /abs/output [--target-dir /abs/cargo-target]
Builds both frontends and all release binaries; writes a tar.gz, SHA256 and BUILD.json.
Requires a clean Git checkout. Does not create a Git tag or publish a release.
HELP
}

version=
output_dir=
repo_root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
target_dir="$repo_root/target"
while (($#)); do
  case "$1" in
    --version|--output-dir|--target-dir)
      (($# >= 2)) || { usage >&2; exit 2; }
      case "$1" in
        --version) version="$2" ;;
        --output-dir) output_dir="$2" ;;
        --target-dir) target_dir="$2" ;;
      esac
      shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+$ ]] || { usage >&2; exit 2; }
[[ -n "$output_dir" ]] || { usage >&2; exit 2; }
[[ "$(uname -s)" == Linux ]] || { echo 'Alpha bundles currently target Linux only.' >&2; exit 1; }
for tool in cargo rustc node npm python3 tar sha256sum realpath; do
  command -v "$tool" >/dev/null || { echo "Missing prerequisite: $tool" >&2; exit 1; }
done
if [[ -n "$(git -C "$repo_root" status --porcelain --untracked-files=normal)" ]]; then
  echo 'Refusing to label a dirty checkout as a reproducible release. Commit/stash your intended changes first.' >&2
  exit 1
fi
source_sha="$(git -C "$repo_root" rev-parse HEAD)"
target_host="$(rustc -vV | sed -n 's/^host: //p')"
[[ "$target_host" == *-linux-* ]] || { echo "Unsupported host target: $target_host" >&2; exit 1; }
output_dir="$(realpath -m "$output_dir")"
target_dir="$(realpath -m "$target_dir")"
case "$output_dir/" in
  "$repo_root/"*) echo 'Output directory must be outside the source checkout.' >&2; exit 1 ;;
esac
name="neige-calm-$version-linux-$(uname -m)"
bin_dir="$target_dir/$target_host/release"
mkdir -p "$output_dir"
for suffix in tar.gz tar.gz.sha256 BUILD.json; do
  [[ ! -e "$output_dir/$name.$suffix" ]] || { echo "Output already exists: $name.$suffix" >&2; exit 1; }
done
# A per-output reservation prevents two builders from publishing over each other.
reservation="$output_dir/.$name.building"
mkdir "$reservation" || { echo "Build already reserved: $reservation" >&2; exit 1; }
staging="$(mktemp -d "$output_dir/.alpha-build.XXXXXXXX")"
trap 'rm -rf -- "$staging"; rmdir -- "$reservation"' EXIT
bundle="$staging/$name"
mkdir -p "$bundle"

cd "$repo_root"
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  CARGO_TARGET_DIR="$target_dir" NEIGE_BUILD_SHA="$source_sha" \
  cargo build --locked --release --target "$target_host" \
  -p calm-server -p calm-codex-bridge -p neige-app -p neige-mcp-stdio-shim \
  -p calm-proc-supervisor -p neige-cli \
  --bin calm-server --bin neige-codex-bridge --bin neige-app \
  --bin neige-mcp-stdio-shim --bin calm-proc-supervisor --bin neige
(cd web && npm ci --legacy-peer-deps && npm run build)
(cd fe && npm ci && npm run build)
"$bin_dir/neige-app" system package \
  --release-dir "$bundle/release" --release-id "$version" \
  --app-bin "$bin_dir/neige-app" \
  --web-dist "$repo_root/web/dist" --fe-dist "$repo_root/fe/web/dist" \
  --bin "calm-server=$bin_dir/calm-server" \
  --bin "calm-proc-supervisor=$bin_dir/calm-proc-supervisor" \
  --bin "neige-codex-bridge=$bin_dir/neige-codex-bridge" \
  --bin "neige-mcp-stdio-shim=$bin_dir/neige-mcp-stdio-shim" \
  --bin "neige=$bin_dir/neige"
python3 - "$bundle/BUILD.json" "$version" "$source_sha" "$target_host" <<'PY'
import datetime, json, pathlib, platform, subprocess, sys
path, version, sha, target = sys.argv[1:]
info = {
    'releaseId': version, 'sourceCommit': sha, 'target': target,
    'builtAt': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'platform': platform.platform(), 'architecture': platform.machine(),
    'libc': list(platform.libc_ver()),
    'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
    'node': subprocess.check_output(['node', '--version'], text=True).strip(),
}
pathlib.Path(path).write_text(json.dumps(info, indent=2) + '\n')
PY
mkdir "$bundle/docs"
cp docs/alpha-release.md docs/deploy-and-upgrade.md docs/neige-app-config.md docs/upgrade-stability.md "$bundle/docs/"
printf '# Install Neige Calm Alpha\n\nFollow [the installation runbook](docs/alpha-release.md).\n' > "$bundle/INSTALL.md"
# Stop if a formatter/generator or another writer changed the source during the build.
[[ "$(git rev-parse HEAD)" == "$source_sha" && -z "$(git status --porcelain --untracked-files=normal)" ]] || {
  echo 'Checkout changed during build; refusing to publish artifacts.' >&2; exit 1;
}
tar -C "$staging" -czf "$staging/$name.tar.gz" "$name"
cp "$bundle/BUILD.json" "$output_dir/$name.BUILD.json"
mv "$staging/$name.tar.gz" "$output_dir/$name.tar.gz"
(cd "$output_dir" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
printf 'Built %s\nSource %s\n' "$output_dir/$name.tar.gz" "$source_sha"
