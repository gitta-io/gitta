# Gitta PR check (GitHub Action)

The Action checks each open pull request against the other open pull requests that target the same branch. It works the same whether the PRs come from cloud agents (Codex, Copilot, Jules, Devin) or from people, and it changes nothing about how you review and merge.

It posts one comment on the PR and keeps it updated:

- **Breaks together with #123**: with a gate set, the two PRs are merged on top of the base branch and the tests fail on the combination.
- **Does not merge with #123**: the two PRs touch the same code in incompatible ways.
- **Potentially affected**: one PR changed a function, attribute or constant that the other PR's new code uses (Python, TypeScript/JavaScript, Go, Rust, Java).

```yaml
# .github/workflows/gitta.yml
name: Gitta
on:
  pull_request: { types: [opened, synchronize, reopened] }
  push: { branches: [main] }        # re-check every open PR when the base moves
permissions: { contents: read, pull-requests: write }
concurrency: { group: gitta-pr-check, cancel-in-progress: false }   # one check at a time keeps one comment per PR
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: your-org/gitta/gitta-action@main
        with:
          version: 0.1.0                  # a released binary: the check starts in seconds
          gate: "npm ci && npm test"
          mode: ${{ github.event_name == 'push' && 'all' || 'current' }}
```

## Inputs

- **`version`**: a released Gitta version to download (checksum verified). Leave it empty to build from source instead, which needs a Rust toolchain on the runner and takes a few minutes the first time.
- **`release-url`**: where to download releases from, if not the action's own GitHub releases.
- **`gate`**: the test command for each combination. Leave it empty for warnings and merge checks only.
- **`mode`**:
  - `current`: check the triggering PR;
  - `all`: check every open PR, useful on a push to the base branch or on a schedule.
- **`fail-on`**:
  - `breaks` (default): fail the check when a combination breaks or doesn't merge;
  - `never`: comment only.
