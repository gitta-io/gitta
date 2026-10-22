# gitta

gitta coordinates AI coding agents that work in one repository at the same time. It tells an agent when another agent changes code that its new work uses, and it tests the combined code inside your normal `git merge`.

## Set up a repository

Run `gitta init` once, with the command that runs your tests. gitta calls this command the gate.

```bash
cd your-repo
gitta init --gate "npm test"
```

`gitta init` does this:

- It finds every agent worktree in `git worktree list`, and every worktree that an agent tool makes later.
- It adds the git hooks `pre-merge-commit`, `reference-transaction`, `post-merge` and `pre-push`. Your own hooks stay and still run first. With git older than 2.28 it adds `post-rewrite` instead of `reference-transaction`.
- It adds two hooks for Claude Code to `~/.claude/settings.json`, and an MCP entry for Cursor and Codex when they are installed.

To share the gate and the rules with your team and the CI check, run `gitta init --team` and commit the `.gitta.toml` it writes.

## Work as usual

Start your agents the way you always do. When one agent changes a function that another agent's new code calls, the second agent reads a note at its next step:

```
Gitta: agent ci-level changed `wilson`, which your new code in bench/aidev-replay/analyze.py calls. Check that your code still fits. Their version: bench/aidev-replay/analyze.py:23: def wilson(k, n, *, z=1.96):

Handle it yourself and tell the user in one sentence.
```

Merge with git. gitta runs the gate on the combined code before main moves. When the tests pass, it adds nothing to the output. When they fail, it stops the merge:

```
$ git merge --no-edit ci99
Auto-merging bench/aidev-replay/analyze.py
gitta: tests fail on main with ci99 (test_ci99.T.test_wider); main unchanged, merge cancelled (ignore git's note). Fix it and merge again.
    TypeError: wilson() takes 2 positional arguments but 3 were given
Now on main: bench/aidev-replay/analyze.py:23: def wilson(k, n, *, z=1.96):
Not committing merge; use 'git commit' to complete the merge.
```

`git pull`, `git pull --rebase`, a fast-forward, a cherry-pick, a rebase and `git push` to the protected branch get the same check.

## What gitta checks

| | |
|---|---|
| **Changed meaning** | gitta reads Python, TypeScript, JavaScript, Go, Rust, Java, C#, Kotlin, C and C++ with Tree-sitter. When one agent changes a function, method, constant or field that another agent's new code uses, the second agent hears about it once, with the new definition line. This is information for the agent. The tests at merge time decide. |
| **Also editing** | Two agents changed the same function or lines. gitta merges both changes by structure, for example by function, import or JSON key. |
| **Merge check** | Inside `git merge`, `git pull` and `git push`, gitta runs the gate on the combined code and stops the merge when the tests fail. |
| **Weakened checks** | Deleted tests, fewer assertions, new skip markers and relaxed CI are reported in the merge output, `.gitta/gitta.log` and the audit trail. `weakened_checks` in `.gitta.toml` decides whether the merge goes ahead (`warn`, the local default) or stops (`block`, the CI default). |
| **Security** | Risky new lines, such as SQL or shell commands built from variables, and security controls that one agent removes while another agent's new code relies on them. |
| **Credentials** | New lines that look like keys, tokens or passwords in code. |
| **Agent instructions** | Changes to `AGENTS.md`, `CLAUDE.md` and similar files. `gitta scan` finds instruction files that a package planted in your dependencies. |
| **Audit trail** | A hash-chained record of every agent, warning, check and merge in `.gitta/audit.jsonl`. `gitta audit verify` checks it. |

gitta never asks you anything. Inside an agent's tool call it prints only what changes the agent's next step. Everything else goes to the log.

## Watch it work

```bash
gitta watch                # a live terminal view, q quits
gitta watch --once         # today's counts and recent events
tail -f .gitta/gitta.log   # the same events as plain lines
gitta status               # agents, changed files and warnings
```

## Commands

```
gitta init [--gate CMD] [--onto BRANCH] [--team] [--weakened-checks warn|block]   set up once
gitta watch [--once]                          what gitta does, as it happens
gitta status [--json]                         agents, changes and warnings
gitta sync <agent> [--from main|<agent>]      merge work from main or another agent into an agent's copy
gitta check <agent> [--with <agent>]          run the gate on the combination, on request
gitta claim|release <agent> <symbol|path> [note]
gitta pr-check --pr REF --other REF...        the pull request check, as CI runs it
gitta run --task T... [--agent claude|codex] [--cmd TEMPLATE]   one agent per task, each in its own workspace
gitta up -n N [--backend worktree|overlay|fuse]                 N agent workspaces, each on its own branch
gitta land <agent>|--all [--pr]               optional: check, merge and move main in one command
gitta approve <agent> <reason>                optional: a recorded override at a person's terminal
gitta install claude|cursor|codex             hooks for Claude Code, MCP for Cursor and Codex
gitta mcp                                     the MCP server for Cursor and Codex
gitta scan [dirs]                             planted agent instructions in dependencies
gitta audit [verify] [--since DATE] [--json|--csv]
gitta update [--check]                        install the latest release
gitta down                                    remove the workspaces gitta made
gitta uninstall [--everywhere]                remove gitta from this repository
```

`gitta <command> --help` shows one command's options.

## Agents

- **Claude Code**: hooks after every tool call and when the agent stops. `gitta install claude` adds them again.
- **Cursor and Codex**: MCP tools (`gitta_status`, `gitta_sync`, `gitta_check`, `gitta_claim`, `gitta_release`) that the agent calls. Tell the agent in `AGENTS.md` to merge with git and to fix a failed merge.
- **Any other tool that makes git worktrees**: `gitta status`, `gitta watch` and the merge output.

## Workspaces on Linux

`gitta up -n 3 --backend overlay` (as root) or `--backend fuse` makes copy-on-write workspaces over one shared checkout. Each agent still gets its own git worktree and branch, `gitta/a1`, `gitta/a2` and so on. After `gitta init`, new Claude Code worktrees share the checkout too. `gitta init --no-cow` turns this off.

## Building

```bash
cargo build --release --manifest-path gitta-cli/Cargo.toml        # gitta
cargo build --release --manifest-path gitta-fs/Cargo.toml         # gitta-fs (Linux, FUSE)
```
