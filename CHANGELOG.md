# Changelog

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
