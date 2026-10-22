#!/bin/bash
# One command per cloud: VM up (Terraform), gitta built from the pinned commit, the published pair list replayed,
# results pulled back, every resource destroyed and the destruction verified with the cloud CLI.
#   [VCPU=8|4] bench/aidev-replay/run.sh gcp|azure|aws [all|up|setup|start|wait|pull|down|verify]
# VCPU=8: 8 vCPU / 32 GB, 3 pairs at a time. VCPU=4: 4 vCPU / 16 GB, 2 pairs at a time. Same container limits per test.
# Needs: terraform; for gcp: gcloud logged in and GCP_PROJECT=<project id>; for azure: az login; for aws: AWS credentials
# (AWS_PROFILE is used when set). SSH_KEY (default ~/.ssh/id_ed25519) with its .pub next to it.
set -euo pipefail
CLOUD=${1:?cloud: gcp|azure|aws}; STEP=${2:-all}
HERE=$(cd "$(dirname "$0")" && pwd); TF=$HERE/../../infra/bench; GITTA_DIR=$(cd "$HERE/../.." && pwd)
GITTA_COMMIT=${GITTA_COMMIT:-$(git -C "$GITTA_DIR" rev-parse HEAD)}
GCP_PROJECT=${GCP_PROJECT:-}; AWS_REGION=${AWS_REGION:-eu-north-1}; SSH_KEY=${SSH_KEY:-$HOME/.ssh/id_ed25519}
[ "$CLOUD" != gcp ] || [ -n "$GCP_PROJECT" ] || { echo "set GCP_PROJECT"; exit 2; }
VCPU=${VCPU:-8}; RUN=$CLOUD$VCPU; WORKERS=$([ "$VCPU" = 8 ] && echo 3 || echo 2)
TFVARS=(-var cloud="$CLOUD" -var vcpu="$VCPU" -var gcp_project="$GCP_PROJECT" -var aws_region="$AWS_REGION" -var aws_profile="${AWS_PROFILE:-}" -var ssh_public_key="$SSH_KEY.pub")
OUT=$HERE/results/$RUN; mkdir -p "$OUT"
KH=$OUT/.known_hosts
SSHO=(-o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KH" -o ConnectTimeout=15 -o ServerAliveInterval=30 -i "$SSH_KEY")
tf() {
  # every provider is configured on every run, so the Google one always needs a fresh token
  [ "$CLOUD" != gcp ] || export GOOGLE_OAUTH_ACCESS_TOKEN=$(gcloud auth print-access-token 2>/dev/null)
  TF_WORKSPACE=$RUN nice terraform -chdir="$TF" "$@"
}
# workspace per run via TF_WORKSPACE (never `workspace select`: runs on several clouds share this directory)
ws() { terraform -chdir="$TF" workspace list | grep -qw "$RUN" || terraform -chdir="$TF" workspace new "$RUN" >/dev/null; }
host() { ws; tf output -raw ssh; }
log() { echo "$(date -u +%FT%TZ) [$RUN] $*" | tee -a "$OUT/timeline.log"; }

up() {
  ws; log "terraform apply"; [ -d "$TF/.terraform" ] || terraform -chdir="$TF" init -input=false >/dev/null
  tf apply -input=false -auto-approve "${TFVARS[@]}" >"$OUT/terraform-apply.log" 2>&1 || { tail -20 "$OUT/terraform-apply.log"; exit 1; }
  tf output -json machine >"$OUT/machine.json"; log "vm up: $(cat "$OUT/machine.json")"
  H=$(host); for i in $(seq 60); do ssh "${SSHO[@]}" "$H" true 2>/dev/null && break; sleep 10; done
  log "ssh ok"; date -u +%s >"$OUT/t_up"
}
setup() {
  H=$(host); T=$(mktemp -d)
  git -C "$GITTA_DIR" archive --format=tar.gz -o "$T/gitta-src.tar.gz" "$GITTA_COMMIT" gitta-core gitta-cli
  echo "$GITTA_COMMIT" >"$T/GITTA_COMMIT"
  ssh "${SSHO[@]}" "$H" mkdir -p bench
  scp -q "${SSHO[@]}" "$T/gitta-src.tar.gz" "$T/GITTA_COMMIT" "$HERE/runner.py" "$HERE/images.json" "$HERE/pairs.jsonl" "$HERE/vm-setup.sh" "$H:bench/"
  rm -rf "$T"; log "setup start"
  ssh "${SSHO[@]}" "$H" 'bash bench/vm-setup.sh' >"$OUT/setup.log" 2>&1 || { tail -30 "$OUT/setup.log"; exit 1; }
  log "setup done"
}
start() {
  H=$(host); log "runner start ${BENCH_LIMIT:+(limit $BENCH_LIMIT)}"
  ssh "${SSHO[@]}" "$H" "cd bench && { [ -n '${BENCH_LIMIT:-}' ] && head -n ${BENCH_LIMIT:-0} pairs.jsonl > run-pairs.jsonl || cp pairs.jsonl run-pairs.jsonl; } && \
    setsid nohup bash -c 'date -u +%s > /data/results/t_start; BENCH_WORKERS=$WORKERS python3 runner.py run run-pairs.jsonl /data/results/results.jsonl > /data/results/runner.log 2>&1; date -u +%s > /data/results/t_end; touch /data/results/DONE' >/dev/null 2>&1 < /dev/null &"
}
wait_done() {
  H=$(host)
  until ssh "${SSHO[@]}" "$H" test -f /data/results/DONE 2>/dev/null; do
    sleep 300; ssh "${SSHO[@]}" "$H" 'wc -l < /data/results/results.jsonl 2>/dev/null; tail -1 /data/results/runner.log' 2>/dev/null | tr '\n' ' ' | sed "s/^/$(date -u +%T) /" >>"$OUT/progress.log" || true
    echo >>"$OUT/progress.log"
  done
  log "runner done"
}
pull() {
  H=$(host)
  ssh "${SSHO[@]}" "$H" 'cd /data/results && tar -czf /tmp/results.tgz results.jsonl runner.log env.json t_start t_end logs'
  scp -q "${SSHO[@]}" "$H:/tmp/results.tgz" "$OUT/results.tgz"
  tar -xzf "$OUT/results.tgz" -C "$OUT"; cp "$OUT/results.jsonl" "$HERE/results-$RUN.jsonl"
  log "pulled $(wc -l <"$OUT/results.jsonl") results"
}
down() {
  ws; log "terraform destroy"
  tf destroy -input=false -auto-approve "${TFVARS[@]}" >"$OUT/terraform-destroy.log" 2>&1 || { tail -20 "$OUT/terraform-destroy.log"; exit 1; }
  date -u +%s >"$OUT/t_down"; log "destroyed"
}
verify() {
  ws; { echo "terraform state: $(tf state list | wc -l | xargs) resources"
  case $CLOUD in
    gcp) echo "instances: $(gcloud compute instances list --project "$GCP_PROJECT" --filter='name~gitta-bench' --format='value(name)' | wc -l | xargs)"
         echo "disks: $(gcloud compute disks list --project "$GCP_PROJECT" --filter='name~gitta-bench' --format='value(name)' | wc -l | xargs)"
         echo "firewall rules: $(gcloud compute firewall-rules list --project "$GCP_PROJECT" --filter='name~gitta-bench' --format='value(name)' | wc -l | xargs)"
         echo "static addresses: $(gcloud compute addresses list --project "$GCP_PROJECT" --format='value(name)' | wc -l | xargs)";;
    azure) echo "resource group gitta-bench exists: $(az group exists -n gitta-bench)"
         echo "resources tagged/named gitta-bench: $(az resource list --query "[?contains(name,'gitta-bench')].name" -o tsv | wc -l | xargs)";;
    aws) echo "instances (not terminated): $(aws ec2 describe-instances --region "$AWS_REGION" --filters Name=tag:Name,Values=gitta-bench Name=instance-state-name,Values=pending,running,stopping,stopped,shutting-down --query 'length(Reservations[].Instances[])' --output text)"
         echo "volumes available/in-use (all): $(aws ec2 describe-volumes --region "$AWS_REGION" --query 'length(Volumes)' --output text)"
         echo "security group gitta-bench-ssh: $(aws ec2 describe-security-groups --region "$AWS_REGION" --filters Name=group-name,Values=gitta-bench-ssh --query 'length(SecurityGroups)' --output text)"
         echo "key pair gitta-bench: $(aws ec2 describe-key-pairs --region "$AWS_REGION" --filters Name=key-name,Values=gitta-bench --query 'length(KeyPairs)' --output text)"
         echo "elastic IPs: $(aws ec2 describe-addresses --region "$AWS_REGION" --query 'length(Addresses)' --output text)";;
  esac; } | tee "$OUT/verify.txt"
}

case $STEP in
  all) trap 'down; verify' EXIT; up; setup; start; wait_done; pull; nice python3 "$HERE/analyze.py" "$HERE" || true;;
  up) up;; setup) setup;; start) start;; wait) wait_done;; pull) pull;; down) down;; verify) verify;;
  *) echo "unknown step $STEP"; exit 2;;
esac
