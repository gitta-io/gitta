#!/bin/bash
# Runs on the fresh Ubuntu 24.04 VM: tools, gitta build from the uploaded source, pinned images, environment record.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
sudo apt-get update -q && sudo apt-get install -yq docker.io git python3 build-essential curl jq >/dev/null
sudo usermod -aG docker "$USER"
sudo mkdir -p /data/work /data/results && sudo chown -R "$USER" /data
curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain 1.90.0
. "$HOME/.cargo/env"
mkdir -p ~/gitta-src && tar -xzf ~/bench/gitta-src.tar.gz -C ~/gitta-src
(cd ~/gitta-src/gitta-cli && cargo build -q --release --locked)
sudo install ~/gitta-src/gitta-cli/target/release/gitta /usr/local/bin/gitta
for i in $(jq -r '.[]' ~/bench/images.json); do sudo docker pull -q "$i"; done
jq -n --arg os "$(. /etc/os-release; echo $PRETTY_NAME)" --arg kernel "$(uname -r)" --arg git "$(git --version)" \
  --arg docker "$(sudo docker --version)" --arg rustc "$(rustc --version)" --arg python "$(python3 --version)" \
  --arg gitta_commit "$(cat ~/bench/GITTA_COMMIT)" --arg gitta_sha256 "$(sha256sum /usr/local/bin/gitta | cut -d' ' -f1)" \
  --arg cpu "$(nproc) x $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)" --arg mem "$(free -g | awk '/Mem/{print $2}') GB" \
  --arg pairs_sha256 "$(sha256sum ~/bench/pairs.jsonl | cut -d' ' -f1)" --slurpfile images ~/bench/images.json \
  '{os:$os,kernel:$kernel,git:$git,docker:$docker,rustc:$rustc,python:$python,gitta_commit:$gitta_commit,gitta_sha256:$gitta_sha256,cpu:$cpu,mem:$mem,pairs_sha256:$pairs_sha256,images:$images[0]}' > /data/results/env.json
echo setup done
