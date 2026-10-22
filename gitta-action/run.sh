#!/usr/bin/env bash
# Gitta PR check: compare open pull requests pairwise, post one sticky comment per PR.
# Who goes first: when two open pull requests break together, the older one (lower number) passes with a warning
# and the newer one fails, so the older can merge and the newer re-checks against the new base. Draft pull requests
# and those labelled `gitta-skip` are compared but never make another one fail. Without this rule two incompatible
# pull requests would block each other under branch protection.
# Env: GITTA (binary), GATE (optional test command), MODE (current|all), FAIL_ON (breaks|never),
#      MAX_PRS, CURRENT_PR (number, for MODE=current), GH_TOKEN. Requires a full clone (fetch-depth: 0).
set -euo pipefail
MARKER='<!-- gitta-pr-check -->'
prs=$(gh pr list --state open --limit "${MAX_PRS:-30}" --json number,title,baseRefName,isDraft,labels --jq '.[] | "\(.number)\t\(.baseRefName)\t\(.title)\t\(if .isDraft or ([.labels[].name] | index("gitta-skip")) then "passive" else "active" end)"')
[ -z "$prs" ] && { echo "No open pull requests."; exit 0; }

# Fetch every open PR head into a local ref.
while IFS=$'\t' read -r n base title _; do
  git fetch -q origin "pull/$n/head:refs/gitta/pr/$n" "$base:refs/remotes/origin/$base" 2>/dev/null || git fetch -q origin "pull/$n/head:refs/gitta/pr/$n"
done <<< "$prs"

targets=$([ "${MODE:-current}" = all ] && cut -f1 <<< "$prs" || echo "${CURRENT_PR:?MODE=current needs a pull_request event}")
status=0
for n in $targets; do
  base=$(awk -F'\t' -v n="$n" '$1==n{print $2}' <<< "$prs")
  args=(pr-check --pr "refs/gitta/pr/$n" --name "#$n" --base "origin/$base")
  [ -n "${GATE:-}" ] && args+=(--gate "$GATE")
  # An approving review is the person's approval that needs_approval rules in .gitta.toml ask for
  [ "$(gh pr view "$n" --json reviewDecision --jq .reviewDecision 2>/dev/null || true)" = APPROVED ] && args+=(--approved)
  others=0
  while IFS=$'\t' read -r m mbase mtitle _; do
    [ "$m" = "$n" ] || [ "$mbase" != "$base" ] && continue
    args+=(--other "refs/gitta/pr/$m=#$m $mtitle"); others=$((others+1))
  done <<< "$prs"
  # With no other pull request the check still runs: removed tests and weakened checks are checked on their own.
  [ "$others" -eq 0 ] && echo "#$n: no other open PRs into $base"
  rc=0; report=$("$GITTA" "${args[@]}") || rc=$?
  [ "$rc" -ne 0 ] && [ "$rc" -ne 3 ] && { echo "$report"; exit "$rc"; }
  json=$("$GITTA" "${args[@]}" --json) || true
  # Exit 3: the pull request breaks a team rule in .gitta.toml, or weakens a check while weakened_checks is block.
  [ "$rc" -eq 3 ] && { echo "#$n: breaks a team rule or weakens a check (see the comment)"; status=1; }
  body="$MARKER"$'\n'"$report"$'\n\n'"<sub>Gitta compares open pull requests: shared symbols changed under each other, merges and, with a gate, the tests on each combination. No AI.</sub>"
  # One sticky comment per PR, updated in place.
  # Two runs at the same moment can both create one; keep the oldest and delete the rest.
  ids=$(gh api "repos/{owner}/{repo}/issues/$n/comments" --paginate --jq ".[] | select(.body | startswith(\"$MARKER\")) | .id")
  id=$(head -1 <<< "$ids")
  if [ -n "$id" ]; then gh api -X PATCH "repos/{owner}/{repo}/issues/comments/$id" -f body="$body" >/dev/null
  else gh pr comment "$n" --body "$body" >/dev/null; fi
  for dup in $(tail -n +2 <<< "$ids"); do gh api -X DELETE "repos/{owner}/{repo}/issues/comments/$dup" >/dev/null || true; done
  echo "#$n checked against $others PR(s)"
  # Fail only for a clash with an older, active pull request: that one goes first.
  if [ "${FAIL_ON:-breaks}" = breaks ]; then
    passive=$(awk -F'\t' '$4=="passive"{printf "%s ", $1}' <<< "$prs")
    blocking=$(jq --argjson n "$n" --arg passive " $passive" '[.[] | (.other | capture("^#(?<m>[0-9]+)").m | tonumber) as $m
      | select($m < $n and ($passive | contains(" \($m) ") | not)) | select(.tests == "FAIL" or .merges == false)] | length' <<< "$json")
    [ "$blocking" -gt 0 ] && status=1
  fi
done
exit $status
