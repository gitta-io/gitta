# Changelog

## 0.2.0 — 2026-11-01

### Tests you can trust

- gitta now counts the tests that ran in Go JSON output and JUnit XML reports too, so a quiet drop in tests is caught on more projects.
- Each merge now keeps the result of every test the full suite ran, so you can see later exactly what was tested.
- gitta catches more ways to make tests pass without testing: a test run that ends itself early, a changed test runner, an equality that is always true and weaker assertions.
- When an agent's task and its tests disagree, the agent runs `gitta conflict` or the `gitta_conflict` tool. gitta holds the work for a person, and the tests stay as they are.
- With `verify_done = true`, an agent that is about to finish without running the tests on its final code is sent back once to run them.
- The pull request check now says when a change touches code and no test, and when a new test will fail at random.
- gitta can read the coverage report your tests write and name changed lines no test ran. It can also check that new tests fail without the change, and make small changes to an agent's lines to find the ones your tests miss.

### Merges and teamwork

- An agent's claims end when `gitta land` merges its work or its worktree is removed, so others can take that code at once. When a landing touches code another agent claimed, gitta says so.
- A failed merge names the last version of the work that passed, and tells you when the work fails only together with other work. With `flaky_rerun = true`, a test that fails at random gets one more run, and the agent is not blamed.
- `gitta main-check` finds the change that broke main and the agent that merged it, and can undo it. `gitta tests-for` lists the tests to run first for an agent's work.
- `gitta run --schedule` runs tasks on different files in parallel and holds tasks that touch the same files.
- gitta tells an agent when another agent is doing the same task, or changes shared state its new code uses.
- With `independent_approval`, the person who started an agent cannot approve its change.
- gitta reads the SARIF reports of your static analysis tools and shows each warning on a line the agent changed.
- gitta catches more ways CI can be weakened, such as a job switched off or removed, and names commits that skipped your pre-commit hooks.

### Your code stays with you

- Your source code now stays on your machines. gitta sends the team server a short summary of each agent step, with the names of changed symbols and the paths of touched files, and the server finds the same warnings from that.
- The merge checks are now harder to switch off by accident. Environment variables, a deleted local config file, or a marker set by hand or copied into place, can no longer weaken the checks your team committed, and `GITTA_SKIP` stays the one recorded way to skip.
- The team server token stays off the command line, and gitta sends it over plain http only to this machine unless you allow more.

### Fixes

- `gitta init --no-claude` no longer promises warnings after every agent step, and tells you how to add them later.
- When only the inside of a function another agent uses changed, gitta no longer calls it probably fine. It asks the agent to check that the function still does what its code expects.
- When a file does not merge, gitta now says why in plain words instead of an internal code.
- When `gitta land` stops on failing tests, the failure record now holds the code that was tested, so `gitta failure rerun` confirms the same failure.
- Projects with submodules now pass the gate when they pass in your own checkout. gitta gives its test checkout the submodules from your main checkout, with no clone and no network.
- A refused merge prints each finding once, with clearer notes and paths relative to the repository.
- gitta exits quietly when the program that reads its output stops early, as in `gitta init | head -1`.
- The limit on how many tests run at once now holds exactly.
- Two test runs that start at the same moment no longer share one checkout, so one run cannot change the files under the other.

### Changes

- `gitta init --no-mcp` leaves your Cursor and Codex settings alone.
- The code security checks, credential checks and `gitta scan` moved to gitta Enterprise.

## 0.1.3 — 2026-10-25

### Security

- The agent sandbox reaches the network for outbound connections only, through pasta (passt) in a private network namespace. The agent can call out for its API, and nothing on the host can call in, so host-local and abstract sockets are out of reach from inside the sandbox. This closes the residual host-local-socket reach of the earlier bubblewrap-only setup.
- When you ask for the sandbox and it cannot be built (bubblewrap, pasta or unprivileged user namespaces missing), gitta stops with a clear message and runs nothing, rather than fall back to running the agent unprotected.

### Performance

- Reading and analysing code runs about 2.6 times faster, from a faster memory allocator (mimalloc).
- The pull request check reads each side's files at the same time, so it finishes sooner on changes that touch many files.

### Changes

- Limit how many tests run at once. One setting, `--parallel` (or `GITTA_GATE_SLOTS` for a single run), now also bounds the tests gitta runs while merging, not only when it cross-tests one agent against another. Set it when your test suite uses a lot of memory, or shares a port or a database. By default merges stay unlimited, as before. On Windows the limit is reported as unsupported rather than shown as enforced.
- A saved test result is now reused only when the environment matches as well as the code. gitta accounts for the toolchains the gate uses, the dependencies installed in the linked folders, and the variables named in `gate_env`, so a changed environment re-runs the gate instead of trusting an old result.
- A gate can be given a time limit (`gate_timeout`, or `GITTA_GATE_TIMEOUT` for a single run). A gate stopped at the limit is never treated as a pass and never reused, and the whole gate process group is stopped.
- Recorded integration failures. When a merge's tests fail, gitta keeps a package with the exact revisions, the gate command, the environment fingerprint, the failing output and a command to re-run the check. `gitta failure list`, `show` and `rerun`, and the `gitta_failures` agent tool, let another agent pick up the repair. A rerun confirms the failure as reproduced only when the environment still matches.
- SHA-256 now uses a pure-Rust implementation, with one fewer third-party dependency.

## 0.1.2 — 2026-10-24

### Security

- The agent sandbox (`gitta run --sandbox` and `gitta-sandbox`) is rebuilt on unprivileged Linux namespaces (bubblewrap). It builds a minimal world from only the agent's worktree, the paths passed with `--write`, and the toolchain, and runs the agent there. The home folder, the environment, host sockets (including the Docker socket and the ssh-agent socket) and other processes are not present in that world, so the agent and every process it starts cannot reach them. The agent keeps full read and write access to its worktree and reaches the network for its API; `--no-net` runs with no network. This replaces the earlier Landlock approach, which confined the agent inside the real host filesystem and had to exclude each host resource one by one. The sandbox now needs the `bubblewrap` package and unprivileged user namespaces (on by default on modern Linux). The underlying finding was reported by Joachim Bauernberger.

## 0.1.1 — 2026-10-24

Security and correctness fixes. The security findings and most of the patches were reported by Joachim Bauernberger.

### Security

- The gate committed in `.gitta.toml` on the target branch now takes precedence over a local one, in every command and in the git hooks. A machine with no local gate can no longer merge past a gate the team committed. Only a person sets the local gate: `gitta init`, `up` and `adopt` refuse `--gate` inside an agent session, and every change to the local gate is recorded in the audit trail.
- An agent's plain commit on the target branch is now checked like a merge — the gate, the team rules in `.gitta.toml`, and the scan for removed tests and weakened checks all run. The agent-session check uses the full variable list, so a Cursor agent is covered as well as Claude Code and Codex.
- Credentials are refused on every route that reaches the target branch, not only `gitta land`. The git hooks stop a credential on a line a branch adds, and `gitta pr-check` fails with exit code 3 regardless of `weakened_checks`.
- `gitta init` no longer writes its hooks into a hooks folder outside the repository, such as a global `core.hooksPath` or husky's in-tree hooks. It stops and explains how to give the repository its own hooks folder.
- Text from other agents, the team server and the test output is quoted and length-limited when it reaches an agent, and each note tells the agent to read quoted text as information, not as instructions.

### Fixes

- The structural merge driver keeps both insertions at one place only when each is a whole object or dict entry. Two insertions of statements, shorthand properties or lines of a string now conflict, as they do in Git.
- The audit trail reads its tail as bytes, so a non-ASCII name or an event larger than 64 KiB no longer restarts the hash chain or trips a false "audit trail BROKEN".
- The credential check lowercases in ASCII only, so a line with a letter such as the Turkish `İ` no longer crashes the check or hides a credential.
- The C/C++ analyzer no longer panics on a file that is not valid UTF-8 near a parameter name.

### Docs and packaging

- `install.sh` states in its header that it edits the shell startup file when the install directory is not on `PATH`.
- The README install section matches the install guide.

## 0.1.0 — 2026-10-22

First release.
