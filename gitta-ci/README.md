# Gitta check for GitLab and Azure Repos

The same check as the GitHub Action, for merge requests on GitLab and pull requests on Azure Repos. Every open
merge request is compared with the others that target the same branch: Gitta reports when one changes something
another one's new code relies on, when two do not merge, and (with a test command) when their combination fails
the tests. One comment per merge request, kept up to date. Nothing leaves your CI; no AI involved.

`GITTA_RELEASES` below is the address where Gitta's release files are published (install script, binaries,
`gitta-mr-check.sh` and these CI files).

## GitLab

```yaml
# .gitlab-ci.yml
include:
  - remote: "$GITTA_RELEASES/gitta.gitlab-ci.yml"
variables:
  GITTA_RELEASES: "<release address>"
  GATE: "npm ci && npm test"        # optional
```

Add a masked CI/CD variable `GITTA_TOKEN`: a project or group access token with the `api` scope, so the check can comment.
It runs on merge-request pipelines (that merge request) and on the default branch (all open merge requests, since the target moved).

## Azure Repos

```yaml
# azure-pipelines.yml, run as a build validation policy on the target branch
steps:
  - template: gitta-pr-check.yml        # copied from the release into your repository
    parameters:
      releases: "<release address>"
      gate: "npm ci && npm test"        # optional
```

Give the project's build service "Contribute to pull requests" on the repository so it can comment.

## Options

- `GATE`: test command run on each combination. Without it: warnings and merge checks only.
- `FAIL_ON`: `breaks` (default) fails the pipeline when a combination breaks or does not merge; `never` only comments.
- `MAX_PRS`: most open merge requests to compare (default 30).

## Tested

`test/run-mock-test.sh` runs the check end to end against real Git repositories and a stand-in for both REST
APIs (`test/mock_api.py`): comments are created, kept to one per merge request across runs, and breaks are
reported. It has not yet run against real gitlab.com or dev.azure.com.
