# Vault format (version 2)

An sbrain vault is a folder of Markdown and TOML files that holds a coding agent's long-term memory: who the
user is, the rules they taught it, and per project the current state, open work and decisions. Agents read it
through the `sbrain` CLI and its hooks; people can read and edit every file by hand.

This document is the canonical description of the format. Code, templates and the guide written into each
vault (`AGENTS.md`) follow it.

## Principles

- **One canonical record per topic.** Answers come from records that are kept up to date, not from session
  logs.
- **Code is the source of truth.** Records point at files and commits; whether a record is stale is measured
  with git.
- **Unverified work is not done.** `done-unverified` and `verified` are different states; only a passing
  `verify` command sets `verified`.
- **Say what is missing.** Recall reports what it read and what it could not find.
- **Context carries pointers, not bulk.** The session-start block stays small so it is never cut silently.
- **Fixed structure, free content.** Field names, status values and the headings the code reads are English
  and fixed. What is written under them can be in any language.
- **Portable.** Nothing machine-specific is stored in tracked files: no absolute paths, no user names in the
  structure. A vault can be synced between machines with git.

## Layout

```
<vault>/
  .sbrain/
    vault.toml              format version and settings
    cache/                  machine-local state (not tracked): repo paths, hook state, logs
  .gitignore                ignores .sbrain/cache/
  AGENTS.md                 how an agent works with this vault (generated)
  CLAUDE.md                 `@AGENTS.md`, for agents that read CLAUDE.md
  README.md                 what this folder is, for people
  profile/
    user.md                 who the user is and how they work
    rules.md                rules that apply to every project
  notes/                    knowledge that belongs to no single project
  projects/
    <project>/
      project.toml          id prefix and repository remotes
      state.md              current state, kept short
      gotchas.md            project-specific pitfalls and rules
      items/                one file per bug, feature, idea or debt
      decisions/            one file per decision
      sessions/             handoff cards
```

A folder is a vault when `.sbrain/vault.toml` exists. Choosing an empty folder creates the whole layout. A
folder with other content is never touched. A folder with `projects/` but no `.sbrain/vault.toml` is a format 1
vault and is not opened; its records can be imported (see below).

Empty `items/`, `decisions/` and `sessions/` folders are not created; the first record creates its folder.

### Files in the vault are never followed out of it

A vault can be synced from a git remote, and a remote can put a symlink anywhere in it. sbrain reads and
writes nothing through one: below the vault's root no part of a path may be a symlink, temporary files
are created only where nothing is, logs are appended without following a link, and ids, remotes and
commit ids from records are checked before they become file names, TOML or git arguments.

### History in git

When git is installed, a new vault is a repository from its first save: sbrain runs `git init` (unless the
empty folder already is a repository, such as a clone of an empty remote) and commits the layout as
`sbrain vault`. Without a git identity of the user's, the vault's own git config gets `sbrain` as the author;
the user's global config is never changed. An empty folder inside another repository is refused, so the
vault keeps a history of its own. An existing vault is used as it is, with or without git. Without git,
nothing of this happens and nothing is said about it.

sbrain commits only the files it writes (the sweep's records, `sbrain item add|set|verify`, `decision
add|set`, `session write`) and the files an agent edits in the vault with its edit tools once they follow
the format (the PostToolUse hook; not `.sbrain/`), each in a commit of its own, only when the vault is the root of its repository, and never pushes; backing up and
syncing is the user's push. A commit git refuses (another git holds the index, a hook says no) waits in
the job ledger and every later sbrain command tries it again, so nothing sbrain wrote stays uncommitted.

## `.sbrain/vault.toml`

```toml
format = 2

[projects]
# Folders that hold unrelated sub-projects: `<folder>/<container>/<name>` is project `<name>`.
containers = []

[hooks]
# Context usage percentages at which a handoff is suggested, once each per session.
context_thresholds = [70, 85]

[hooks.context_windows]
# Model name fragment → context window in tokens. The longest matching fragment wins.
default = 200000

[sweep]
# `sbrain sweep`: minutes a transcript must be left unwritten before it is distilled, the most sessions
# distilled in one run, and the fewest messages from the user a distilled session has.
settle_minutes = 60
max_per_run = 4
min_user_turns = 2
# Also: timeout_seconds (900), online_check ("api.anthropic.com:443", empty to skip),
# online_timeout_seconds (5), skip_cwd_prefixes (temporary folders such as /tmp/ and /var/folders/).
```

Unknown keys are ignored. A missing or invalid file makes the folder not a vault. A higher `format` than the
binary knows is refused, so a vault is never misread.

## Projects

A project is identified by its folder name under `projects/`. The project for a working directory is found
in this order:

1. Inside the vault: no project.
2. Not accessible (manual mode, outside every listed folder): skipped.
3. The repository's `origin` remote matches a `repos` entry in some `project.toml`: that project.
4. Manual mode, under a listed folder: a listed git repository is the project; otherwise the first folder
   under it (or `<container>/<name>`).
5. Auto mode, in a git repository: named after the repository's main folder (worktrees resolve to their main
   repository). If a project of that name already belongs to another remote, the owner is appended:
   `<name>-<owner>`.
6. Auto mode, outside git: the nearest folder (the working directory or one above it, below the home folder)
   whose name is a project the vault already has. A folder that holds a project's repositories without
   being one itself (`shop/` around `shop-ios/`) is found this way; a new plain folder is not a project
   until a project of its name exists.

On session start the project folder is created when it does not exist, with `project.toml`, `state.md` and
`gotchas.md` from the templates, so a new repository is remembered from its first session.

### `project.toml`

```toml
# Id prefix: items are <prefix>-0001, decisions <prefix>-D-0001.
prefix = "api"
# Normalized git remotes of this project's repositories (host/owner/name).
repos = ["github.com/acme/api"]
```

Remotes are normalized so the ssh and https forms match: `git@github.com:acme/api.git` and
`https://github.com/acme/api` are both `github.com/acme/api`. A project may span several repositories.

The local path of each project's repository is not stored in the vault; it is remembered per machine in
`.sbrain/cache/repos.json` when a session runs there, and used to check staleness.

## Records

Every record is Markdown with a `---` frontmatter of `key: value` lines; `[a, b]` is a list. Dates are
`YYYY-MM-DD`, times `YYYY-MM-DD HH:MM`, local time.

### Item (`items/<id>-<slug>.md`)

```yaml
id: api-0031
title: 401 after login
type: bug | feature | idea | debt
status: inbox | planned | doing | done-unverified | verified | dropped
created: 2026-10-24
updated: 2026-10-24
aliases: [auth error, token expiry]   # extra search terms
files: [src/auth/session.ts]          # staleness is checked against these
commit: a3f9c21                       # the commit that did the work
verify: npm test -- auth              # command that proves it works
verified_at:                          # set when verify passes
related: [api-0009, api-D-0004]
captured_during: checkout screen work # what was going on when it came up
```

Body: `## Problem`, `## Cause`, `## Solution`, `## Verification`, `## Notes`.

Status rules:
- Work the user sets aside for later → `inbox`; the agent goes back to what it was doing.
- Work the agent takes on → `doing`; when finished `done-unverified`; when `verify` passes `verified` with `verified_at`.
- `verified` is never written by hand; only a passing `verify` (`sbrain item verify <id>`) sets it. A failing one takes a `verified` item back to `done-unverified`.
- `verified` and `dropped` are closed. Closed records stay where they are, so links never break.

The slug is the title folded to ASCII, lowercase, words joined by `-`, at most 40 characters.

### Decision (`decisions/<prefix>-D-<n>-<slug>.md`)

```yaml
id: api-D-0004
title: Keep the session in a cookie
status: accepted | superseded
superseded_by:
created: 2026-10-24
files: [src/auth/]
commit:                               # HEAD when the decision was taken
related: [api-0031]
```

Body: `## Context`, `## Decision`, `## Why`, `## Alternatives`.

### Handoff card (`sessions/<YYYY-MM-DD-HHMM>-<session8>.md`)

```yaml
session: 1a2b3c4d                     # first 8 characters of the agent's session id
project: api
date: 2026-10-24 14:30
source: handoff | transcript
```

Body: `## Done`, `## Decisions`, `## Open`, `## Next step`, `## Touched records`. A card written by
`sbrain sweep` (`source: transcript`) also has `## Uncertain`, ending with a line saying it was distilled
and not verified.

Cards are sources, not answers: what they say is folded into `state.md`, items and decisions. A card for the
same session is rewritten in place.

### `state.md`

```yaml
project: api
updated: 2026-10-24
```

Body: `## What`, `## Architecture`, `## Working`, `## Not working`, `## Run and test`. At most about 2000
characters; details belong in items and decisions. When it disagrees with the code, the code wins and the
file is corrected.

### `gotchas.md`, `profile/rules.md`

One rule per line, dated: `- 2026-10-24: Read a file's current content before changing it.` A correction
from the user becomes a rule right away: project-specific ones in `gotchas.md`, general ones in
`profile/rules.md`. A rule that contradicts an older one replaces it.

### `proposed-rules.md`

Rules the sweep finds in transcripts are not put in effect: text in a session (a tool's output, a file it
read) can pass for a correction from the user, and a rule reaches every later session. They wait in
`proposed-rules.md` beside `gotchas.md`, or in `profile/` for every project, one per line with the session
they came from: `- 2026-10-26 (1a2b3c4d): Measure before caching.` No agent reads the file; session start
only says how many wait. `sbrain rule list` shows them with an id, `sbrain rule accept <id>` moves one
into `gotchas.md` or `profile/rules.md`, `sbrain rule reject <id>` drops it.

### `profile/user.md`

Free text about the user: language, tone, tools, how they like to work. It is read at every session start.

## Memory layers

| Layer | Files | Lifecycle |
| --- | --- | --- |
| Working | `state.md`, open items | updated continuously |
| Episodic | `sessions/` | written once |
| Semantic | `decisions/`, `notes/` | grows by distilling; replaced through `superseded` |
| Procedural | `profile/rules.md`, `gotchas.md` | grows with corrections |

## Session-start context

The session-start hook prints, within 6000 characters: the project and repository, `profile/user.md`,
`profile/rules.md`, the project's `state.md`, `gotchas.md`, open work (counts, doing, unverified, planned,
inbox, possibly stale verified items, the last handoff's next step), the doctor's summary when it found
problems, and a short protocol pointing at `AGENTS.md`. A file that does not fit is cut and named, so the
agent knows to open it. Possibly stale items are what the last full `sbrain doctor` found (`stale.json`):
checking runs git for every file of every verified item, so session start never checks itself, and starts
the doctor in the background when that result is a day old.

Nothing is written to the user's global agent instructions (such as `~/.claude/CLAUDE.md`); everything an
agent needs arrives through the hook and the vault's own `AGENTS.md`.

## Machine-local state (`.sbrain/cache/`)

| Path | Content |
| --- | --- |
| `repos.json` | project → local repository path, last seen |
| `hooks/<session>.json` | the last context threshold reported in that session |
| `hook-errors.log` | hook failures (hooks never fail a session) |
| `doctor.json` | the doctor's last result |
| `stale.json` | verified items whose files changed, as the last full doctor run found them |
| `hook-slow.log` | hooks that took over 250 ms, the last 50; the doctor reports them |
| `distill/` | working folder of the transcript distiller; its sessions get no hook context |
| `sweep.json` | when sweeping started, and the last run |
| `ledger.sqlite` | the job ledger: each session's distilling, sessions reported as ended, and vault commits that failed, with their attempts, last error and next try |
| `sweep.lock` | held while `sbrain sweep` runs, so two runs never overlap |

The ledger (SQLite) is how nothing that has to happen is lost: a job that fails is tried again after a
wait that grows each time (1 minute, 5 minutes, 30 minutes, 2 hours, 12 hours, then daily) and is given up
after 8 failures, which `sbrain doctor` reports. Deleting it loses only that history: the sweep looks at
the transcripts again, from the time in `sweep.json`.

## Transcript sweep

`sbrain sweep` is the safety net for sessions that end without a handoff card. It reads the transcripts of
every connected agent that keeps them (an agent sbrain cannot be connected to, such as Codex, counts when it
is installed), and of the engine even when it is not connected, and distills each one into records through
the engine CLI. Claude Code keeps `<config dir>/projects/<folder>/<session>.jsonl`; Codex keeps
`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<session>.jsonl` (`~/.codex` by default).

1. The first run only notes the time; sessions written before it are never distilled.
2. A transcript is due once it has not been written for `settle_minutes`, or as soon as the agent's
   SessionEnd hook reported the session as ended, and has grown since it was last handled. A failed one
   is tried again; one that was distilled is not, until it grows.
3. Skipped: sessions with fewer than `min_user_turns` messages from the user, in a temporary folder, in a
   folder that is no project, or covered by a handoff card written after the user's last message.
4. Before the first distilling of a run, `online_check` must answer; without a connection the run stops
   and nothing counts as failed.
5. The engine runs in a `distill` folder beside sbrain's settings, outside the vault (so no project
   settings a remote synced into the vault apply), without tools, hooks or a saved session, and answers
   with JSON only. sbrain checks the answer and writes the records itself: new items (never `verified`),
   changes to the project's open items, decisions, proposed rules (`proposed-rules.md`, not in effect),
   `state.md` while it is still the template, and the session's card with `source: transcript`. An answer
   that does not pass the checks writes nothing. In the transcript it reads, the text of tools and of the
   assistant is indented, so none of it can pass for the user's own words.
6. When the vault is a git repository, the files written for a session are committed together.

At most `max_per_run` sessions are distilled in one run.

## Importing a format 1 vault

A format 1 vault is never opened; its records are imported into a format 2 vault instead (`sbrain import
<folder>`). The source is only read. The target only gains
files: a file that already exists is left alone (identical content counts as already imported, anything
else is reported as a conflict), except `profile/user.md` and `profile/rules.md` while they still hold
their templates. Running it again writes nothing new.

| Format 1 | Format 2 |
| --- | --- |
| `companion/core.md`, `companion/rules.md` | `profile/user.md`, `profile/rules.md` |
| `knowledge/`, `inbox/`, `archive/` | `notes/`, `notes/inbox/`, `notes/archive/` |
| `projects/<p>/items/` headings Belirti, Kök neden, Çözüm, Doğrulama, Notlar | Problem, Cause, Solution, Verification, Notes |
| `decisions/` headings Bağlam, Karar, Neden, Alternatifler | Context, Decision, Why, Alternatives |
| `sessions/` headings Yapılan, Kararlar, Açık kalan, Belirsiz, Sonraki adım, Dokunulan kayıtlar; `source: devret` | Done, Decisions, Open, Uncertain, Next step, Touched records; `source: handoff` |
| `state.md` headings Ne, Mimari, Çalışıyor, Çalışmıyor / bilinen sorunlar, Nasıl çalıştırılır / test edilir; `repo:` | What, Architecture, Working, Not working, Run and test; `repo:` dropped |
| `project.json`, `repo:` | `project.toml` (prefix from the item ids, remote from the repository's `origin`); the local path goes to `.sbrain/cache/repos.json` |
| `.claude/brain.json` `containers` | `[projects] containers` in `.sbrain/vault.toml` |
| other files under `projects/<p>/` | copied as they are |
| `.claude/`, `.state/`, `templates/`, `skills/`, `agents/`, `prompts/`, `apps/`, `launchd/`, `CLAUDE.md` | not imported (tooling) |

Headings inside code fences are not renamed; a heading with a suffix keeps it (`## Çalışıyor (2026-10-24)` →
`## Working (2026-10-24)`).
