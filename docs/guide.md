# sbrain guide

Everything about sbrain that the README leaves out. The vault's own format is in
[vault-format.md](vault-format.md).

## Install from an archive

Download the archive for your system from the
[latest release](https://github.com/emredagci/sbrain/releases/latest) (`sbrain-<version>-<platform>.tar.gz`; the
Mac binaries are signed and notarized), check it against `SHA256SUMS`, and put `sbrain` in a folder on your
PATH:

```bash
tar -xzf sbrain-*-macos-arm64.tar.gz
```

```bash
install -m 755 sbrain-*/sbrain ~/.local/bin/sbrain
```

## How it works

- **Session start.** A hook gives the agent the user's profile and rules, the project's state, gotchas and
  open work, within 6000 characters, and points it at the vault's `AGENTS.md`.
- **Each prompt.** A hook adds the records related to what was asked and, when the context window fills up,
  reminds the agent to hand off.
- **During the session.** The agent keeps the vault as `AGENTS.md` says: work set aside for later becomes
  an inbox item, work it takes on an item in progress, a choice with its reasons a decision, a correction a
  dated rule. Before the session ends it writes a handoff card with the next step.
- **Sessions without a card.** `sbrain sweep` reads the transcripts of sessions that ended without a handoff
  card and has your coding CLI distill them into the same records.
- **Health.** `sbrain doctor` finds broken records, secrets in the vault, work left unverified or forgotten,
  verified work whose files changed since, links that point nowhere, context files that get cut, and backup
  problems.
- **Secrets stay out.** Keys, tokens and passwords are masked as `[redacted]` in what the engine reads and in
  every record sbrain or an agent writes, before the vault is committed.

The vault format, from the records' fields to the session-start context, is described in
[docs/vault-format.md](vault-format.md).

## First run

No path is assumed. The vault and the accessible folders are chosen together:

```bash
sbrain setup set --vault <vault-path> --auto
```

```bash
sbrain setup set --vault <vault-path> --folder <path> --folder <path>
```

The choice is saved in the OS settings directory (macOS: `~/Library/Application Support/sbrain/settings.json`,
Linux: `~/.config/sbrain/settings.json`). `SBRAIN_VAULT` overrides the saved vault.

### The vault

Choosing an empty folder sets up a new vault from the templates: `.sbrain/vault.toml`, `profile/`, `notes/`,
`projects/`, an `AGENTS.md` that tells agents how to read and keep the memory, and a README for people. An
existing vault is opened as it is (its `AGENTS.md` is brought up to date); a folder with other content is
refused and left alone.

With git installed, a new vault keeps its history from the first save: sbrain runs `git init` (an empty clone
of a remote keeps its remote) and commits the layout. An empty folder inside another repository is refused, so
the vault keeps a history of its own. sbrain commits only what it writes, one commit each, and never pushes; a
commit git refuses is tried again by every later sbrain command. Add a remote and push to back the vault up or
share it between machines. Without git, none of this happens. A format 1 vault (a folder with `projects/`
but no `.sbrain/vault.toml`) is not opened; import its records into a new vault instead. The source is only read and nothing in
the vault is overwritten:

```bash
sbrain import <format-1-vault>
```

```bash
sbrain import <format-1-vault> --yes
```

The first shows what would be written, the second writes it.

A project's folder is created the first time a session starts in its repository. It is named after the
repository and remembers the repository's git remote, so the same project is recognized on another machine
or in another clone.

### Accessible folders

Access is checked when a request arrives; nothing is scanned.

- **Auto mode:** every folder is allowed. There is no folder list; a project is named after its git
  repository, and a folder that is not in a repository is not a project.
- **Manual mode:** only folders inside a listed folder are allowed; anything else is refused. A listed git
  repository is a project itself, otherwise each sub-folder is a project.

```bash
sbrain access auto
```

```bash
sbrain access manual <path> <path>
```

In manual mode, `sbrain access add <path>` and `sbrain access remove <path>` edit the list.

## Coding agents

`sbrain clients` lists the coding CLIs on this machine (Claude Code, Codex) and whether sbrain is connected.
`sbrain engine` shows which of them runs sbrain's own work (the sweep) and with which model and effort, and the
choices; `sbrain engine set claude --model sonnet --effort medium` changes them.

Connecting Claude Code (`sbrain connect claude` shows the plan; `--yes` applies it) makes these changes:

- SessionStart, UserPromptSubmit, SessionEnd, PostToolUse (after Edit and Write) and PreCompact hooks in
  `~/.claude/settings.json` that run `sbrain hook …`
- In `permissions.allow`, rules for reading the memory and writing records (`sbrain project`, `items`,
  `doctor`, `item add|set`, `decision add|set`, `session write`, `rule list` and the like). Everything
  that changes what later sessions see still asks you: choosing the vault and folders, connecting, the sweep and its schedule, `item verify`
  (it runs the command a record names) and `rule accept`.
- `~/.local/bin/sbrain`, a link to the CLI, unless `sbrain` is already on PATH

The hooks run the `sbrain` on PATH when that is the program connecting (Homebrew's `/opt/homebrew/bin/sbrain`,
which stays put across upgrades), else the program's own path. Each event has one sbrain hook: connecting from
another sbrain points the existing hooks at it in place, and replaces sbrain hooks that run any other sbrain.

Nothing is added to `CLAUDE.md`: the session-start hook carries everything an agent needs and points at the
vault's `AGENTS.md`.

What was added is recorded next to sbrain's settings (`connections/claude.json`); `sbrain disconnect claude`
removes exactly that. Things that already existed are left alone, and each file is copied to
`*.sbrain-backup` before its first change. `CLAUDE_CONFIG_DIR` and `SBRAIN_BIN_DIR` redirect the targets.
Codex cannot be connected; its sessions are swept when it is installed, and it can be the engine
([Sweep](#sweep)).

## Hooks

`sbrain hook session-start` and `sbrain hook prompt` are Claude Code's SessionStart and UserPromptSubmit
hooks: they read the payload on stdin and print `additionalContext`. Session start gives the user profile,
rules, the project's state, gotchas and open work, within 6000 characters; a prompt gets related records and,
when the context fills up, a reminder to hand off. `sbrain hook session-end` prints nothing: it notes that the
session ended, so the sweep distills it right away instead of waiting for it to settle. `sbrain hook
tool-use` runs after the agent edits or writes a file: when the file is an item or decision in the vault
and something is wrong with it (missing fields, a status or type that does not exist, an id already taken,
a link to nothing), the agent is told what to fix at once. Before that, anything in the file that looks
like a secret is replaced with `[redacted]` and the agent is told. A file in the vault that is fine is
committed, so hand edits join the history like sbrain's own writes; files outside the vault get no answer.

`sbrain hook pre-compact` runs before Claude Code compacts a conversation. A summary can drop what the user
asked along the way, and the sweep has not distilled the session yet, so the user's own messages are saved
word for word, secrets masked, to `.sbrain/cache/compact/<session>.md` (machine-local, removed after 14 days).
The session start that follows the compaction adds them to its context under "Kept through compaction",
within 3500 characters: messages that read like instructions or corrections first ("from now on", "never",
"bundan sonra", "yazma"), then the newest, shown in the order they were written. The instructions among
them also come with every prompt for the rest of the session, within 1500 characters, so the summary
cannot push them out of view.

Every `sbrain` command except the hooks keeps the connection working. A connection missing one of the hooks
gets it back, recorded like the others, so `sbrain disconnect` removes it too. When the sbrain the hooks run
is gone, the hooks move to the
`sbrain` on PATH if that is the one running; otherwise sbrain says on stderr that the hooks are broken and how to
reconnect. A build run from its own folder never takes the hooks over by itself.

A hook never fails the session: errors go to `.sbrain/cache/hook-errors.log` in the vault and nothing is
printed. Hooks run before every prompt; one that takes over 250 ms is noted in `.sbrain/cache/hook-slow.log`, and `sbrain doctor` says so.

## Sweep

`sbrain sweep` is the safety net for sessions that end without a handoff card.

- The first run only notes the time; sessions before it are never distilled.
- Transcripts come from every connected agent, from Codex when it is installed (it cannot be connected),
  and from the engine.
- A transcript is due once it has not been written for an hour, or as soon as its session ended, and has
  grown since it was last handled.
  Short sessions, temporary folders, folders that are no project and sessions covered by a later handoff
  card are skipped.
- Your coding CLI (the one `sbrain engine` names, with its model) runs outside the vault, without tools,
  hooks or a saved session, and answers with JSON only. Codex has no switch for running without tools,
  so it runs in a read-only sandbox without MCP servers. sbrain checks the answer and writes the records
  itself; an answer that does not pass writes nothing.
- Rules it finds are only proposed, never put in effect: a tool's output in a session could pass for a
  correction from you. Session start says how many wait; `sbrain rule list`, `sbrain rule accept <id>`
  and `sbrain rule reject <id>` decide.
- A run distills at most four sessions (`[sweep] max_per_run`), and a lock keeps two runs apart.
- A session that fails is tried again after a wait that grows each time (1 minute up to a day), or at
  once if it went on; after 8 failures it is given up and `sbrain doctor` says so. `sbrain sweep retry
  [<session>]` tries it again. The job ledger (`.sbrain/cache/ledger.sqlite`) keeps all this per
  computer.

```bash
sbrain sweep --dry-run
```

```bash
sbrain sweep
```

To have this computer sweep by itself:

```bash
sbrain sweep schedule --every 3
```

It installs a launchd job on macOS (`~/Library/LaunchAgents/com.emredagci.sbrain.sweep.plist`, logging to
`sweep.log` beside sbrain's settings) or a systemd user timer on Linux (`sbrain-sweep.timer`, logging to the
journal). Every 15 minutes the job runs `sbrain sweep --due`, which runs only when a run is due: every
`--every` hours (1, 3, 6, 12 or 24; saved in sbrain's settings, default 1), right away when a session
ended, and a few minutes after a run that left sessions for later. A run missed while the computer slept
happens once it wakes. A due run distills up to `--per-run` sessions (2, 4 or 8; default 4). The job runs the
`sbrain` on PATH, so upgrades keep it working; if its sbrain is gone, the next `sbrain` command found on PATH
points it at itself. `sbrain sweep unschedule` removes exactly what was installed, and `sbrain doctor` notes a
vault that is swept but not scheduled here, or a sweep that stopped running.

The sweep's settings in the vault are under `[sweep]` in `.sbrain/vault.toml`.

## Commands

| Command | What it does |
| --- | --- |
| `sbrain setup`, `sbrain setup set` | Show or choose the vault and accessible folders |
| `sbrain engine`, `sbrain engine set <client>` | Show or choose the CLI, model and effort that run sbrain's own work |
| `sbrain access …` | Show or change the accessible folders |
| `sbrain project [--cwd <path>]` | The project a folder belongs to |
| `sbrain clients`, `sbrain connect <client>`, `sbrain disconnect <client>` | Coding agents, and connecting sbrain to them |
| `sbrain hook session-start`, `sbrain hook prompt`, `sbrain hook session-end`, `sbrain hook tool-use` | Claude Code's hooks |
| `sbrain item add`, `sbrain item set <id> key=value` | Write or change items |
| `sbrain decision add`, `sbrain decision set <id> key=value` | Write or change decisions; `--supersedes <id>` replaces one |
| `sbrain session write --session <id>` | Write a session's handoff card |
| `sbrain item verify <id> [--repo <folder>] [--timeout <s>]` | Run the item's verify command; a pass makes it `verified` |
| `sbrain overview`, `sbrain items`, `sbrain decisions`, `sbrain sessions`, `sbrain graph` | The vault as JSON |
| `sbrain uninstall [--yes]` | Remove what sbrain set up on this computer except the vault; then remove the program |
| `sbrain status [--json]` | Everything sbrain does on this computer and what needs you; changes nothing |
| `sbrain fact set <key> <value> [--from <date>] [--to <date>]`, `fact end <key>`, `fact list [--at <date>] [--all]` | Facts with the dates they held; a new value closes the old one (`--general` for no single project) |
| `sbrain event add <what> [--when <date>] [--kind <word>] [--status done\|planned\|cancelled]`, `event list` | Things that happened once or are planned |
| `sbrain timeline [--kind <word>] [--since <date>] [--json]` | Events in order with counts, gaps and how long ago, computed for today |
| `sbrain doctor [--quick] [--json]` | Check the vault; changes nothing |
| `sbrain sweep [--due] [--dry-run] [--json]` | Distill sessions that ended without a handoff card |
| `sbrain sweep schedule [--every <hours>] [--per-run <n>]`, `sbrain sweep unschedule` | Have this computer run the sweep by itself |
| `sbrain sweep retry [<session>]` | Try again a session the sweep gave up on |
| `sbrain rule list`, `sbrain rule accept <id>`, `sbrain rule reject <id>` | Rules the sweep proposed, and putting one in effect |
| `sbrain import <folder> [--yes]` | Import a format 1 vault |

`sbrain <command> --help` describes each one.

## Privacy

sbrain sends nothing: no usage stats, no crash reports, no update checks. The CLI and the
hooks make no network request, except one: before distilling, the sweep opens a connection to `[sweep]
online_check` in `.sbrain/vault.toml` (by default `api.anthropic.com:443`) to tell whether the computer is
online, and sends nothing over it; an empty value skips the check.

The sweep is the one place where session content leaves this computer, and it goes through your own coding
CLI, not through sbrain: it hands a digest of each session's transcript to that CLI, which sends it to its
provider under your account, as your own sessions do, and it counts toward your usage. `sbrain sweep
unschedule` makes it run only when you run `sbrain sweep`.

## Code layout

```
crates/
  sbrain-core/      the vault and everything done to it: setup and git history, projects, access,
                    records, search, staleness, doctor, distilling and the sweep's bookkeeping. Knows no
                    agent. templates/ holds the vault, project, record and distilling templates.
  sbrain-clients/   agent adapters, one module per client (claude/, codex/): detection, connecting,
                    hooks, transcripts and running its CLI
apps/
  sbrain/           the CLI (see Commands); schedule.rs installs the sweep's launchd or systemd job
packaging/
  homebrew/         the Homebrew formula's template, filled in by the release workflow
xtask/              development tasks (cargo xtask)
```

Dependencies point one way: `core ← clients`, `apps/*` on top.

Long-running work, such as the transcript sweep, lives behind the CLI as `sbrain` commands; the OS's own
scheduler runs them in the background.

The CLI is the one interface: hooks call it, coding agents call it through their shell, people use it
directly. There is no MCP server.

## Development

```bash
cargo test
```

```bash
cargo clippy --all-targets
```

On Linux, in Docker: the CI steps on Ubuntu 24.04, then in a container booting systemd the scheduled sweep
end to end (connect, schedule, a session reported as ended and distilled by the job with a stand-in
`claude` in `~/.local/bin`, status, unschedule, disconnect). It builds for the Docker host's architecture.

```bash
./.github/linux-check.sh
```

To try the first run again, `cargo xtask reset` disconnects the coding agents, removes the scheduled sweep
and moves sbrain's settings and caches to the Trash. It lists what it will do and asks first. `--vault` adds
the vault in the settings (only an sbrain vault); `--delete` deletes instead; `--yes` does not ask.

```bash
cargo xtask reset
```

## Releasing

A version tag publishes a release: `.github/workflows/release.yml` builds the CLI for macOS (Apple Silicon
and Intel) and Linux (x86_64 and arm64, on Ubuntu 22.04 so older glibc works) with
`.github/package.sh`, one `sbrain-<version>-<platform>.tar.gz` each (`macos-arm64`, `macos-x86_64`, `linux-arm64`,
`linux-x86_64`). The Mac binaries are signed with the
Developer ID (hardened runtime) and notarized by Apple; a lone binary cannot be stapled, so Gatekeeper
checks the ticket online. The release gets the archives, `SHA256SUMS` and `sbrain.rb`, the Homebrew formula
for them, rendered from `packaging/homebrew/sbrain.rb.in`. With the `HOMEBREW_TAP_TOKEN` secret (a token that
can push to `emredagci/homebrew-tap`), the formula is committed to the tap as `Formula/sbrain.rb`. Set the version in
`Cargo.toml` first; the tag's message becomes the release notes.

The jobs that sign and publish run in the `release` environment. Keep the signing, notarization and tap
secrets there rather than at repository level, and give the environment required reviewers and a
deployment rule for version tags, `[0-9]*` (Settings › Environments): then no push or tag alone can sign a build. The
workflows pin every action to a commit and do not keep the checkout's token.

```bash
git tag -a 0.2.0 -m "What changed, as a Markdown list"
```

```bash
git push origin 0.2.0
```

Running the workflow by hand without a tag builds, signs and notarizes the branch and keeps the archives
as the run's artifacts, publishing nothing. One archive can be built locally too (unsigned unless
`SIGN_IDENTITY` names a Developer ID in the keychain):

```bash
TARGET=aarch64-apple-darwin ./.github/package.sh
```

## Reporting problems

Open an issue: [github.com/emredagci/sbrain/issues](https://github.com/emredagci/sbrain/issues/new?template=bug.yml),
with the output of `sbrain --version` and `sbrain clients`, and your OS.
