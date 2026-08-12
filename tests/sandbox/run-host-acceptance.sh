#!/usr/bin/env bash
set -euo pipefail

project_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
config=${VENTO_FIRECRACKER_CONFIG:?set VENTO_FIRECRACKER_CONFIG to the host config JSON}
if [[ -f "$HOME/.cargo/env" ]]; then source "$HOME/.cargo/env"; fi

test -r /dev/kvm
test -w /dev/kvm
test "$(stat -fc %T /sys/fs/cgroup)" = cgroup2fs

data_dir=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["dataDir"])' "$config")
mkdir -p "$data_dir"
filesystem=$(findmnt -n -o FSTYPE -T "$data_dir")
case "$filesystem" in xfs|btrfs) ;; *) echo "dataDir must use XFS or Btrfs, got $filesystem" >&2; exit 1;; esac
source_file="$data_dir/.acceptance-source"
clone_file="$data_dir/.acceptance-clone"
trap 'rm -f "$source_file" "$clone_file"' EXIT
dd if=/dev/zero of="$source_file" bs=1M count=4 status=none
cp --reflink=always "$source_file" "$clone_file"

cd "$project_root"
cargo test -p vento-firecracker-runtime tests::real_host_preflight_accepts_provisioned_environment -- --ignored --exact
echo "host acceptance passed"
