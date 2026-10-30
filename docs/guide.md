# reprise guide

Everything about reprise that the README leaves out. The vault's own format is in
[vault-format.md](vault-format.md).

## Install from an archive

Download the archive for your system from the
[latest release](https://github.com/emredagci/reprise/releases/latest) (`reprise-<version>-<platform>.tar.gz`; the
Mac binaries are signed and notarized), check it against `SHA256SUMS`, and put `reprise` in a folder on your
PATH:

```bash
tar -xzf reprise-*-macos-arm64.tar.gz
```

```bash
install -m 755 reprise-*/reprise ~/.local/bin/reprise
```

## How it works

- **Session start.** A hook gives the agent the user's profile and rules, the project's state, gotchas and
  open work, within 6000 characters, and points it at the vault's `AGENTS.md`.
- **Each prompt.** A hook adds the records related to what was asked and, when the context window fills up,
  reminds the agent to hand off.
- **During the session.** The agent keeps the vault as `AGENTS.md` says: work set aside for later becomes
  an inbox item, work it takes on an item in progress, a choice with its reasons a decision, a correction a
  dated rule. Before the session ends it writes a handoff card with the next step.
- **Sessions without a card.** `reprise sweep` reads the transcripts of sessions that ended without a handoff
  card and has your coding CLI distill them into the same records.
- **Health.** `reprise doctor` finds broken records, secrets in the vault, work left unverified or forgotten,
  verified work whose files changed since, links that point nowhere, context files that get cut, and backup
  problems.
- **Secrets stay out.** Keys, tokens and passwords are masked as `[redacted]` in what the engine reads and in
  every record reprise or an agent writes, before the vault is committed.

The vault format, from the records' fields to the session-start context, is described in
[docs/vault-format.md](vault-format.md).

## First run

No path is assumed. The vault and the accessible folders are chosen together:

```bash
reprise setup set --vault <vault-path> --auto
```

```bash
reprise setup set --vault <vault-path> --folder <path> --folder <path>
```

The choice is saved in the OS settings directory (macOS: `~/Library/Application Support/reprise/settings.json`,
Linux: `~/.config/reprise/settings.json`). `REPRISE_VAULT` overrides the saved vault.

### The vault

Choosing an empty folder sets up a new vault from the templates: `.reprise/vault.toml`, `profile/`, `notes/`,
`projects/`, an `AGENTS.md` that tells agents how to read and keep the memory, and a README for people. An
existing vault is opened as it is (its `AGENTS.md` is brought up to date); a folder with other content is
refused and left alone.

With git installed, a new vault keeps its history from the first save: reprise runs `git init` (an empty clone
of a remote keeps its remote) and commits the layout. An empty folder inside another repository is refused, so
the vault keeps a history of its own. reprise commits only what it writes, one commit each, and never pushes; a
commit git refuses is tried again by every later reprise command. Add a remote and push to back the vault up or
share it between machines. Without git, none of this happens. A format 1 vault (a folder with `projects/`
but no `.reprise/vault.toml`) is not opened; import its records into a new vault instead. The source is only read and nothing in
the vault is overwritten:

```bash
reprise import <format-1-vault>
```

```bash
reprise import <format-1-vault> --yes
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
reprise access auto
```

```bash
reprise access manual <path> <path>
```

In manual mode, `reprise access add <path>` and `reprise access remove <path>` edit the list.

## Coding agents

`reprise clients` lists the coding CLIs on this machine (Claude Code, Codex) and whether reprise is connected.
`reprise engine` shows which of them runs reprise's own work (the sweep) and with which model and effort, and the
choices; `reprise engine set claude --model sonnet --effort medium` changes them.

OpenRouter can be the engine too: `reprise engine set openrouter --model <id>` distills with any model it
serves (`typesafe/jev-router` among them). The key is `OPENROUTER_API_KEY`, or the file `openrouter.key`
beside reprise's settings, which the scheduled sweep can read. Requests go through the system's `curl`, the
key on its stdin. OpenRouter only distills: it keeps no transcripts to sweep.

Every record the engine returns carries quotes from the session and a confidence from 0 to 1; reprise
writes it only when a quote is found and the confidence is at least 0.6 (0.8 for a rule), and lists the
rest on the session's card.

Connecting Claude Code (`reprise connect claude` shows the plan; `--yes` applies it) makes these changes:

- SessionStart, UserPromptSubmit, SessionEnd, PostToolUse (after Edit and Write) and PreCompact hooks in
  `~/.claude/settings.json` that run `reprise hook …`
- In `permissions.allow`, rules for reading the memory and writing records (`reprise project`, `items`,
  `doctor`, `item add|set`, `decision add|set`, `session write`, `rule list` and the like). Everything
  that changes what later sessions see still asks you: choosing the vault and folders, connecting, the sweep and its schedule, `item verify`
  (it runs the command a record names) and `rule accept`.
- `~/.local/bin/reprise`, a link to the CLI, unless `reprise` is already on PATH

The hooks run the `reprise` on PATH when that is the program connecting (Homebrew's `/opt/homebrew/bin/reprise`,
which stays put across upgrades), else the program's own path. Each event has one reprise hook: connecting from
another reprise points the existing hooks at it in place, and replaces reprise hooks that run any other reprise.

Nothing is added to `CLAUDE.md`: the session-start hook carries everything an agent needs and points at the
vault's `AGENTS.md`.

What was added is recorded next to reprise's settings (`connections/claude.json`); `reprise disconnect claude`
removes exactly that. Things that already existed are left alone, and each file is copied to
`*.reprise-backup` before its first change. `CLAUDE_CONFIG_DIR` and `REPRISE_BIN_DIR` redirect the targets.
Codex cannot be connected; its sessions are swept when it is installed, and it can be the engine
([Sweep](#sweep)).

## Status line, server and meaning search

- **Status line.** `reprise statusline` prints `reprise · <project> · <what it last did>` (context loaded,
  N related records recalled, messages kept through a compaction, a vault file saved or a secret masked),
  from what the hooks noted in `.reprise/cache/pulse/`. In `~/.claude/settings.json`:
  `"statusLine": {"type": "command", "command": "reprise statusline"}`. Connecting does not set it.
- **Server.** `reprise serve [--port 7448]` serves the vault over HTTP on 127.0.0.1 for other tools:
  `GET /v1/status`, `POST /v1/search`, `GET /v1/facts?project=<p>[&at=<date>]`,
  `GET /v1/timeline?project=<p>`, `POST /v1/hook/<event>`. A request must name `127.0.0.1` or
  `localhost` as its host, and a POST must be `application/json`. Nothing else needs it.
- **Meaning search.** `reprise embeddings set --url <endpoint> --model <name> [--key-env VAR]` points at
  an OpenAI-compatible embeddings endpoint (a local Ollama: `http://localhost:11434/v1/embeddings`);
  `reprise search --semantic` then also lists the records closest in meaning. Vectors are cached in
  `.reprise/cache/vectors.sqlite`; only changed records are sent again, secrets masked.

## Hooks

`reprise hook session-start` and `reprise hook prompt` are Claude Code's SessionStart and UserPromptSubmit
hooks: they read the payload on stdin and print `additionalContext`. Session start gives the user profile,
rules, the project's state, gotchas and open work, within 6000 characters; a prompt gets related records and,
when the context fills up, a reminder to hand off. `reprise hook session-end` prints nothing: it notes that the
session ended, so the sweep distills it right away instead of waiting for it to settle. `reprise hook
tool-use` runs after the agent edits or writes a file: when the file is an item or decision in the vault
and something is wrong with it (missing fields, a status or type that does not exist, an id already taken,
a link to nothing), the agent is told what to fix at once. Before that, anything in the file that looks
like a secret is replaced with `[redacted]` and the agent is told. A file in the vault that is fine is
committed, so hand edits join the history like reprise's own writes; files outside the vault get no answer.

`reprise hook pre-compact` runs before Claude Code compacts a conversation. A summary can drop what the user
asked along the way, and the sweep has not distilled the session yet, so the user's own messages are saved
word for word, secrets masked, to `.reprise/cache/compact/<session>.md` (machine-local, removed after 14 days).
The session start that follows the compaction adds them to its context under "Kept through compaction",
within 3500 characters: messages that read like instructions or corrections first ("from now on", "never",
"bundan sonra", "yazma"), then the newest, shown in the order they were written. The instructions among
them also come with every prompt for the rest of the session, within 1500 characters, so the summary
cannot push them out of view.

Every `reprise` command except the hooks keeps the connection working. A connection missing one of the hooks
gets it back, recorded like the others, so `reprise disconnect` removes it too. When the reprise the hooks run
is gone, the hooks move to the
`reprise` on PATH if that is the one running; otherwise reprise says on stderr that the hooks are broken and how to
reconnect. A build run from its own folder never takes the hooks over by itself.

A hook never fails the session: errors go to `.reprise/cache/hook-errors.log` in the vault and nothing is
printed. Hooks run before every prompt; one that takes over 250 ms is noted in `.reprise/cache/hook-slow.log`, and `reprise doctor` says so.

## Sweep

`reprise sweep` is the safety net for sessions that end without a handoff card.

- The first run only notes the time; sessions before it are never distilled.
- Transcripts come from every connected agent, from Codex when it is installed (it cannot be connected),
  and from the engine.
- A transcript is due once it has not been written for an hour, or as soon as its session ended, and has
  grown since it was last handled.
  Short sessions, temporary folders, folders that are no project and sessions covered by a later handoff
  card are skipped.
- Your coding CLI (the one `reprise engine` names, with its model) runs outside the vault, without tools,
  hooks or a saved session, and answers with JSON only. Codex has no switch for running without tools,
  so it runs in a read-only sandbox without MCP servers. reprise checks the answer and writes the records
  itself; an answer that does not pass writes nothing.
- Rules it finds are only proposed, never put in effect: a tool's output in a session could pass for a
  correction from you. Session start says how many wait; `reprise rule list`, `reprise rule accept <id>`
  and `reprise rule reject <id>` decide.
- A run distills at most four sessions (`[sweep] max_per_run`), and a lock keeps two runs apart.
- A session that fails is tried again after a wait that grows each time (1 minute up to a day), or at
  once if it went on; after 8 failures it is given up and `reprise doctor` says so. `reprise sweep retry
  [<session>]` tries it again. The job ledger (`.reprise/cache/ledger.sqlite`) keeps all this per
  computer.

```bash
reprise sweep --dry-run
```

```bash
reprise sweep
```

To have this computer sweep by itself:

```bash
reprise sweep schedule --every 3
```

It installs a launchd job on macOS (`~/Library/LaunchAgents/com.emredagci.reprise.sweep.plist`, logging to
`sweep.log` beside reprise's settings) or a systemd user timer on Linux (`reprise-sweep.timer`, logging to the
journal). Every 15 minutes the job runs `reprise sweep --due`, which runs only when a run is due: every
`--every` hours (1, 3, 6, 12 or 24; saved in reprise's settings, default 1), right away when a session
ended, and a few minutes after a run that left sessions for later. A run missed while the computer slept
happens once it wakes. A due run distills up to `--per-run` sessions (2, 4 or 8; default 4). The job runs the
`reprise` on PATH, so upgrades keep it working; if its reprise is gone, the next `reprise` command found on PATH
points it at itself. `reprise sweep unschedule` removes exactly what was installed, and `reprise doctor` notes a
vault that is swept but not scheduled here, or a sweep that stopped running.

The sweep's settings in the vault are under `[sweep]` in `.reprise/vault.toml`.

## Commands

| Command | What it does |
| --- | --- |
| `reprise setup`, `reprise setup set` | Show or choose the vault and accessible folders |
| `reprise engine`, `reprise engine set <client>` | Show or choose the CLI, model and effort that run reprise's own work |
| `reprise access …` | Show or change the accessible folders |
| `reprise project [--cwd <path>]` | The project a folder belongs to |
| `reprise clients`, `reprise connect <client>`, `reprise disconnect <client>` | Coding agents, and connecting reprise to them |
| `reprise hook session-start`, `reprise hook prompt`, `reprise hook session-end`, `reprise hook tool-use` | Claude Code's hooks |
| `reprise item add`, `reprise item set <id> key=value` | Write or change items |
| `reprise decision add`, `reprise decision set <id> key=value` | Write or change decisions; `--supersedes <id>` replaces one |
| `reprise session write --session <id>` | Write a session's handoff card |
| `reprise item verify <id> [--repo <folder>] [--timeout <s>]` | Run the item's verify command; a pass makes it `verified` |
| `reprise overview`, `reprise items`, `reprise decisions`, `reprise sessions`, `reprise graph` | The vault as JSON |
| `reprise uninstall [--yes]` | Remove what reprise set up on this computer except the vault; then remove the program |
| `reprise status [--json]` | Everything reprise does on this computer and what needs you; changes nothing |
| `reprise fact set <key> <value> [--from <date>] [--to <date>]`, `fact end <key>`, `fact list [--at <date>] [--all]` | Facts with the dates they held; a new value closes the old one (`--general` for no single project) |
| `reprise event add <what> [--when <date>] [--kind <word>] [--status done\|planned\|cancelled]`, `event list` | Things that happened once or are planned |
| `reprise timeline [--kind <word>] [--since <date>] [--json]` | Events in order with counts, gaps and how long ago, computed for today |
| `reprise doctor [--quick] [--json]` | Check the vault; changes nothing |
| `reprise sweep [--due] [--dry-run] [--json]` | Distill sessions that ended without a handoff card |
| `reprise sweep schedule [--every <hours>] [--per-run <n>]`, `reprise sweep unschedule` | Have this computer run the sweep by itself |
| `reprise sweep retry [<session>]` | Try again a session the sweep gave up on |
| `reprise sweep replay <session>` | Write a distilled session's records again from the engine's kept answer, without the engine |
| `reprise search "<phrasing>" … [--project <name>]` | Hits, partial matches, records one link away and terms nothing matched, as JSON |
| `reprise reset [--on <date>] [--note …]`, `reprise reset --undo` | Leave a project's past out of session start from a day on; nothing is deleted |
| `reprise export <file.tar.gz> [--history]`, `reprise restore <file> <folder>` | Move a vault as one file |
| `reprise rule list`, `reprise rule accept <id>`, `reprise rule reject <id>` | Rules the sweep proposed, and putting one in effect |
| `reprise import <folder> [--yes]` | Import a format 1 vault |

`reprise <command> --help` describes each one.

## Privacy

reprise sends nothing: no usage stats, no crash reports, no update checks. The CLI and the
hooks make no network request, except one: before distilling, the sweep opens a connection to `[sweep]
online_check` in `.reprise/vault.toml` (by default `api.anthropic.com:443`) to tell whether the computer is
online, and sends nothing over it; an empty value skips the check.

The sweep is the one place where session content leaves this computer, and it goes through your own coding
CLI, not through reprise: it hands a digest of each session's transcript to that CLI, which sends it to its
provider under your account, as your own sessions do, and it counts toward your usage. `reprise sweep
unschedule` makes it run only when you run `reprise sweep`.

## Code layout

```
crates/
  reprise-core/      the vault and everything done to it: setup and git history, projects, access,
                    records, search, staleness, doctor, distilling and the sweep's bookkeeping. Knows no
                    agent. templates/ holds the vault, project, record and distilling templates.
  reprise-clients/   agent adapters, one module per client (claude/, codex/): detection, connecting,
                    hooks, transcripts and running its CLI
apps/
  reprise/           the CLI (see Commands); schedule.rs installs the sweep's launchd or systemd job
packaging/
  homebrew/         the Homebrew formula's template, filled in by the release workflow
xtask/              development tasks (cargo xtask)
```

Dependencies point one way: `core ← clients`, `apps/*` on top.

Long-running work, such as the transcript sweep, lives behind the CLI as `reprise` commands; the OS's own
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
and moves reprise's settings and caches to the Trash. It lists what it will do and asks first. `--vault` adds
the vault in the settings (only an reprise vault); `--delete` deletes instead; `--yes` does not ask.

```bash
cargo xtask reset
```

## Releasing

A version tag publishes a release: `.github/workflows/release.yml` builds the CLI for macOS (Apple Silicon
and Intel) and Linux (x86_64 and arm64, on Ubuntu 22.04 so older glibc works) with
`.github/package.sh`, one `reprise-<version>-<platform>.tar.gz` each (`macos-arm64`, `macos-x86_64`, `linux-arm64`,
`linux-x86_64`). The Mac binaries are signed with the
Developer ID (hardened runtime) and notarized by Apple; a lone binary cannot be stapled, so Gatekeeper
checks the ticket online. The release gets the archives, `SHA256SUMS` and `reprise.rb`, the Homebrew formula
for them, rendered from `packaging/homebrew/reprise.rb.in`. With the `HOMEBREW_TAP_TOKEN` secret (a token that
can push to `emredagci/homebrew-tap`), the formula is committed to the tap as `Formula/reprise.rb`. Set the version in
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

Open an issue: [github.com/emredagci/reprise/issues](https://github.com/emredagci/reprise/issues/new?template=bug.yml),
with the output of `reprise --version` and `reprise clients`, and your OS.
