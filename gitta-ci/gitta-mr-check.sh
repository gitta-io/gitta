#!/usr/bin/env bash
# Gitta merge-request check for GitLab and Azure Repos: compare every open merge request with the others
# targeting the same branch and keep one comment per merge request up to date. The engine is the same
# `gitta pr-check` the GitHub check uses; only the platform API differs.
#
# Env: PROVIDER (gitlab|azure, detected from the CI), GITTA (binary, default gitta on PATH), GATE (test command, optional),
#      MODE (current|all), FAIL_ON (breaks|never), MAX_PRS (default 30)
#   GitLab: GITTA_TOKEN (project or group access token with `api` scope); CI_API_V4_URL, CI_PROJECT_ID, CI_MERGE_REQUEST_IID from GitLab CI
#   Azure:  SYSTEM_ACCESSTOKEN (map $(System.AccessToken)); SYSTEM_COLLECTIONURI, SYSTEM_TEAMPROJECT, BUILD_REPOSITORY_ID,
#           SYSTEM_PULLREQUEST_PULLREQUESTID from Azure Pipelines
# Needs git, curl, python3 and a full clone.
set -euo pipefail
GITTA=${GITTA:-gitta}; MAX=${MAX_PRS:-30}; MARKER='<!-- gitta-pr-check -->'
if [ -z "${PROVIDER:-}" ]; then
  if [ -n "${GITLAB_CI:-}" ]; then PROVIDER=gitlab; elif [ -n "${TF_BUILD:-}" ]; then PROVIDER=azure; else PROVIDER=unknown; fi
fi
json() { python3 -c "import json,sys; d=json.load(sys.stdin); $1"; }

case "$PROVIDER" in
gitlab)
  API="${CI_API_V4_URL:?}/projects/${CI_PROJECT_ID:?}"; AUTH=(-H "PRIVATE-TOKEN: ${GITTA_TOKEN:?set GITTA_TOKEN: an access token with api scope}")
  CURRENT=${CI_MERGE_REQUEST_IID:-}
  list()     { curl -fsS "${AUTH[@]}" "$API/merge_requests?state=opened&per_page=$MAX" | json "print('\n'.join('%s\t%s\t%s' % (m['iid'], m['target_branch'], m['title']) for m in d))"; }
  fetch_pr() { git fetch -q origin "+refs/merge-requests/$1/head:refs/gitta/pr/$1"; }
  comments() { curl -fsS "${AUTH[@]}" "$API/merge_requests/$1/notes?per_page=100&sort=asc" | json "print('\n'.join(str(n['id']) for n in d if n['body'].startswith('$MARKER')))"; }
  create()   { curl -fsS "${AUTH[@]}" -X POST "$API/merge_requests/$1/notes" --data-urlencode "body=$2" >/dev/null; }
  update()   { curl -fsS "${AUTH[@]}" -X PUT "$API/merge_requests/$1/notes/$2" --data-urlencode "body=$3" >/dev/null; }
  delete()   { curl -fsS "${AUTH[@]}" -X DELETE "$API/merge_requests/$1/notes/$2" >/dev/null || true; }
  LABEL='!' ;;
azure)
  API="${SYSTEM_COLLECTIONURI:?}${SYSTEM_TEAMPROJECT:?}/_apis/git/repositories/${BUILD_REPOSITORY_ID:?}"; V="api-version=7.1"
  AUTH=(-H "Authorization: Bearer ${SYSTEM_ACCESSTOKEN:?map SYSTEM_ACCESSTOKEN: \$(System.AccessToken)}" -H "Content-Type: application/json")
  CURRENT=${SYSTEM_PULLREQUEST_PULLREQUESTID:-}
  list()     { curl -fsS "${AUTH[@]}" "$API/pullrequests?searchCriteria.status=active&\$top=$MAX&$V" | json "print('\n'.join('%s\t%s\t%s\t%s' % (p['pullRequestId'], p['targetRefName'].removeprefix('refs/heads/'), p['title'], p['sourceRefName']) for p in d['value']))"; }
  fetch_pr() { local src; src=$(awk -F'\t' -v n="$1" '$1==n{print $4}' <<< "$prs"); git fetch -q origin "+$src:refs/gitta/pr/$1"; }
  # A comment id here is "thread/comment"
  comments() { curl -fsS "${AUTH[@]}" "$API/pullRequests/$1/threads?$V" | json "print('\n'.join('%s/%s' % (t['id'], t['comments'][0]['id']) for t in d['value'] if t.get('comments') and str(t['comments'][0].get('content','')).startswith('$MARKER')))"; }
  body_json(){ python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }
  create()   { curl -fsS "${AUTH[@]}" -X POST "$API/pullRequests/$1/threads?$V" -d "{\"comments\":[{\"parentCommentId\":0,\"content\":$(body_json "$2"),\"commentType\":1}],\"status\":1}" >/dev/null; }
  update()   { curl -fsS "${AUTH[@]}" -X PATCH "$API/pullRequests/$1/threads/${2%/*}/comments/${2#*/}?$V" -d "{\"content\":$(body_json "$3")}" >/dev/null; }
  delete()   { curl -fsS "${AUTH[@]}" -X DELETE "$API/pullRequests/$1/threads/${2%/*}/comments/${2#*/}?$V" >/dev/null || true; }
  LABEL='#' ;;
*) echo "gitta: cannot tell the CI platform; set PROVIDER=gitlab or PROVIDER=azure"; exit 2 ;;
esac

prs=$(list)
[ -z "$prs" ] && { echo "No open merge requests."; exit 0; }
while IFS=$'\t' read -r n base _; do fetch_pr "$n"; git fetch -q origin "+refs/heads/$base:refs/remotes/origin/$base" || true; done <<< "$prs"
targets=$([ "${MODE:-current}" = all ] && cut -f1 <<< "$prs" || echo "${CURRENT:?MODE=current needs a merge-request pipeline}")
status=0
for n in $targets; do
  base=$(awk -F'\t' -v n="$n" '$1==n{print $2}' <<< "$prs")
  args=(pr-check --pr "refs/gitta/pr/$n" --name "$LABEL$n" --base "origin/$base")
  [ -n "${GATE:-}" ] && args+=(--gate "$GATE")
  others=0
  while IFS=$'\t' read -r m mbase mtitle _; do
    { [ "$m" = "$n" ] || [ "$mbase" != "$base" ]; } && continue
    args+=(--other "refs/gitta/pr/$m=$LABEL$m $mtitle"); others=$((others+1))
  done <<< "$prs"
  # With no other merge request the check still runs: removed tests and weakened checks are checked on their own.
  [ "$others" -eq 0 ] && echo "$LABEL$n: no other open merge requests into $base"
  rc=0; report=$("$GITTA" "${args[@]}") || rc=$?
  [ "$rc" -ne 0 ] && [ "$rc" -ne 3 ] && { echo "$report"; exit "$rc"; }
  # Exit 3: the merge request removes tests or weakens a check, and .gitta.toml (or the CI default) says block.
  [ "$rc" -eq 3 ] && { echo "$LABEL$n: breaks a team rule or weakens a check (see the comment)"; status=1; }
  [ "$PROVIDER" = gitlab ] && report=$(sed 's/pull request(s)/merge request(s)/g; s/open pull request/open merge request/g' <<< "$report")
  json_out=$("$GITTA" "${args[@]}" --json) || true
  body="$MARKER"$'\n'"$report"$'\n\n'"<sub>Gitta compares open merge requests: shared symbols changed under each other, merges and, with a gate, the tests on each combination. No AI.</sub>"
  ids=$(comments "$n")
  id=$(head -1 <<< "$ids")
  if [ -n "$id" ]; then update "$n" "$id" "$body"; else create "$n" "$body"; fi
  for dup in $(tail -n +2 <<< "$ids"); do delete "$n" "$dup"; done
  echo "$LABEL$n checked against $others merge request(s)"
  # Fail only for a clash with an older merge request (lower number): that one goes first, so two incompatible
  # requests never block each other.
  if [ "${FAIL_ON:-breaks}" = breaks ]; then
    blocking=$(python3 -c 'import json,re,sys; n=int(sys.argv[1]); print(sum(1 for r in json.load(sys.stdin) if (m:=re.match(r"\D*(\d+)", r["other"])) and int(m.group(1)) < n and (r.get("tests")=="FAIL" or r.get("merges") is False)))' "$n" <<< "$json_out")
    [ "$blocking" -gt 0 ] && status=1
  fi
done
exit $status
