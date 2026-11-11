#!/bin/sh
# Install apt packages on a CI runner, retrying the index update and the
# download. A runner whose resolver is briefly down otherwise fails the job
# before any of our code runs.
set -eu
attempt=1
while :; do
  if sudo apt-get -o Acquire::Retries=5 update &&
    sudo env DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y "$@"; then
    exit 0
  fi
  if [ "$attempt" -ge 5 ]; then
    echo "apt install failed after $attempt attempts: $*" >&2
    exit 1
  fi
  sleep $((attempt * 15))
  attempt=$((attempt + 1))
done
