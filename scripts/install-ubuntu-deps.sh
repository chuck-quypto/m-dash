#!/usr/bin/env bash
set -euo pipefail

source /etc/os-release
if [[ "$ID" != ubuntu || "$VERSION_ID" != 24.04 || $# == 0 ]]; then
  echo 'Usage: bash scripts/install-ubuntu-deps.sh PACKAGE... (Ubuntu 24.04)' >&2
  exit 2
fi

work=$(mktemp -d)
trap 'sudo rm -rf -- "$work"' EXIT
mkdir "$work/lists"
chmod 755 "$work" "$work/lists"
apt_options=(
  -o "Dir::Etc::sourcelist=$work/sources.list"
  -o 'Dir::Etc::sourceparts=-'
  -o "Dir::State::lists=$work/lists"
  -o 'Acquire::Retries=1'
  -o 'Acquire::http::Timeout=20'
  -o 'Acquire::https::Timeout=20'
)

for mirror in https://archive.ubuntu.com/ubuntu https://security.ubuntu.com/ubuntu; do
  for suite in noble noble-updates noble-backports noble-security; do
    printf 'deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] %s %s main restricted universe multiverse\n' "$mirror" "$suite"
  done > "$work/sources.list"
  echo "Fetching Ubuntu prerequisites from $mirror"

  # Time out network work before dpkg can start modifying installed packages.
  if sudo timeout --kill-after=10s 90s apt-get "${apt_options[@]}" update --error-on=any &&
     sudo timeout --kill-after=10s 180s apt-get "${apt_options[@]}" --download-only install -y "$@"; then
    sudo env DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" --no-download install -y "$@"
    exit 0
  else
    echo "::warning::Ubuntu metadata/download failed at $mirror (exit $?)"
  fi
done

echo '::error::Both Ubuntu mirrors failed to fetch the required packages.' >&2
exit 1
