# Changelog

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
