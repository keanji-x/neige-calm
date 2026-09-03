#!/usr/bin/env bash

set -euo pipefail

# Install a repository-scoped GitHub Actions runner without disturbing the
# other runner instances on the host. Run this script as the interactive
# administrator account; it obtains the short-lived registration token with
# that account's `gh` login and uses sudo only for host/user service changes.

repo="${REPO:-keanji-x/neige-calm}"
runner_user="${RUNNER_USER:-runner}"
runner_name="${RUNNER_NAME:-$(hostname)-neige-calm-main}"
runner_label="${RUNNER_LABEL:-neige-calm-main}"

runner_home="$(getent passwd "$runner_user" 2>/dev/null | cut -d: -f6 || true)"
if [ -z "$runner_home" ]; then
  echo "error: Linux user '$runner_user' does not exist" >&2
  exit 1
fi
runner_group="$(id -gn "$runner_user")"

source_dir="${SOURCE_RUNNER_DIR:-$runner_home/actions-runner}"
install_dir="${RUNNER_DIR:-$runner_home/actions-runner-neige-calm}"
cargo_target_dir="${RUNNER_CARGO_TARGET_DIR:-$runner_home/target-neige-calm-main}"

for command in gh sudo readlink; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "error: required command '$command' is not installed" >&2
    exit 1
  fi
done

if [ "${EUID:-$(id -u)}" -eq 0 ]; then
  echo "error: run this script as your normal account, not as root" >&2
  exit 1
fi

if [ -e "$install_dir/.runner" ]; then
  echo "error: $install_dir is already configured as a runner" >&2
  exit 1
fi

bin_dir="$(readlink -f "$source_dir/bin")"
externals_dir="$(readlink -f "$source_dir/externals")"
case "$bin_dir" in
  "$source_dir"/bin.*) ;;
  *) echo "error: $source_dir/bin does not point at a versioned runner bin directory" >&2; exit 1 ;;
esac
case "$externals_dir" in
  "$source_dir"/externals.*) ;;
  *) echo "error: $source_dir/externals does not point at a versioned runner externals directory" >&2; exit 1 ;;
esac

runner_files=(
  config.sh
  env.sh
  run-helper.sh.template
  run.sh
  runsvc.sh
  safe_sleep.sh
  svc.sh
)
for file in "${runner_files[@]}"; do
  if [ ! -e "$source_dir/$file" ]; then
    echo "error: runner distribution is missing $source_dir/$file" >&2
    exit 1
  fi
done

# Fail before prompting for sudo if the GitHub login cannot administer the
# target repository. The registration token is short-lived and never written
# to disk by this script.
gh api "repos/$repo" --jq '.permissions.admin' | grep -qx true || {
  echo "error: active gh account is not an administrator of $repo" >&2
  exit 1
}
registration_token="$(gh api --method POST "repos/$repo/actions/runners/registration-token" --jq .token)"
trap 'registration_token=' EXIT HUP INT TERM

sudo -v
sudo install -d -m 0755 -o "$runner_user" -g "$runner_group" "$install_dir"
sudo install -d -m 2775 -o "$runner_user" -g "$runner_group" "$cargo_target_dir"

sudo cp -a "$bin_dir" "$externals_dir" "$install_dir/"
for file in "${runner_files[@]}"; do
  sudo cp -a "$source_dir/$file" "$install_dir/$file"
done
for file in .env .path; do
  if [ -f "$source_dir/$file" ]; then
    sudo cp -a "$source_dir/$file" "$install_dir/$file"
  fi
done

sudo ln -sfn "${bin_dir##*/}" "$install_dir/bin"
sudo ln -sfn "${externals_dir##*/}" "$install_dir/externals"
sudo chown -R "$runner_user:$runner_group" "$install_dir" "$cargo_target_dir"

sudo -u "$runner_user" -H bash -c '
  cd "$1"
  shift
  exec ./config.sh "$@"
' _ "$install_dir" \
  --unattended \
  --url "https://github.com/$repo" \
  --token "$registration_token" \
  --name "$runner_name" \
  --labels "$runner_label" \
  --work _work

# config.sh captures sudo's restricted PATH. Add the runner account's Rust
# tool directory after configuration so service jobs can find cargo/rustup.
sudo -u "$runner_user" -H bash -c '
  path_file="$1/.path"
  rust_bin="$2/.cargo/bin"
  current_path="$(<"$path_file")"
  case ":$current_path:" in
    *":$rust_bin:"*) ;;
    *) printf "%s:%s\n" "$rust_bin" "$current_path" > "$path_file" ;;
  esac
' _ "$install_dir" "$runner_home"

(
  cd "$install_dir"
  sudo ./svc.sh install "$runner_user"
  sudo ./svc.sh start
  sudo ./svc.sh status
)

echo "runner '$runner_name' is installed with label '$runner_label'"
