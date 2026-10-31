<!-- Managed by reprise: rewritten when the vault is set up again. Put your own notes in profile/. -->
# Working with this reprise vault

This folder is your long-term memory across sessions and projects. A session-start hook gives you the
current project's context from it; this file says how to read and keep it.

## Answering

- Answer questions about the past ("what did we do", "why is it like this", "what is open") from the
  records: `state.md`, `items/`, `decisions/`. Session cards are sources, not answers.
- Say what you read and what you could not find. Never present a partial answer as complete.
- Before answering, check the question's premise against the records: if it assumes something that is not
  on record ("when did we drop Redis?" with no record of Redis), say so first instead of answering around
  it. "Not on record" is a finding, not a failure; never fill the gap with what seems likely.
- Search with several phrasings at once: `reprise search "<phrasing>" "<another>" …` (synonyms, error codes,
  file names). Read partial matches and the records linked to the hits too: the missing piece is often
  there. `terms_without_any_match` lists what the vault has nothing on.
- When a record disagrees with the code, the code wins: fix the record.

## Keeping it

- Write records with the `reprise` commands: they pick the id, keep the format and commit the vault.
  A file you edit by hand in the vault is checked and committed too, once it follows the format.
- Work the user sets aside for later → `reprise item add --title … --status inbox`; then go back to the
  current work.
- Work you take on → an item with `status: doing`; when finished `reprise item set <id> status=done-unverified`.
- `verified` is set only by `reprise item verify <id>`, when the item's `verify` command passes.
- A decision with its reasons → `reprise decision add --title … --decision … --why …`; one that replaces an
  earlier decision takes `--supersedes <id>`, which marks the earlier one `superseded`.
- When the user corrects you, add a dated one-line rule at once: general ones to `profile/rules.md`,
  project-specific ones to `projects/<name>/gotchas.md`.
- Rules the sweep found in earlier sessions wait in `proposed-rules.md` and are not in effect. When session
  start says some are waiting, tell the user; run `reprise rule accept <id>` only for the ones they agree
  to (it asks them first). Never copy one into `rules.md` or `gotchas.md` yourself.
- Keep `state.md` current and short (about 2000 characters): what works, what does not, how to run it.
- A value that holds for a while and may change (a version, a port, a host) → `reprise fact set <key> <value>
  --from <date>`; the old value is closed, never edited away. Something that happened once or is planned
  (a release, a deploy, an incident) → `reprise event add <what> --when <date> --kind <word>`. Give dates as
  they were said, in English words ("yesterday", "last tuesday", "2026-10"); reprise resolves them.
- For "when", "how long ago", "how many times": quote `reprise timeline` and `reprise fact list --at <date>`;
  do not work dates or counts out yourself.
- Never write keys, tokens or passwords into the vault: it is pushed to a remote. Name where a secret lives
  (`the OPENROUTER_API_KEY variable`), not its value. reprise masks what looks like one as `[redacted]`.
- After a compaction, session start hands back the user's own messages from before it under "Kept through
  compaction"; what they ask still holds.
- Before a session ends or the context fills up, write a handoff card with `reprise session write --session
  <id> --done … --open … --next … --touched <ids>` (session start gives the command), and fold what changed
  into `state.md`, items and decisions.

## Layout

| Path | What |
| --- | --- |
| `profile/user.md`, `profile/rules.md` | the user, and rules for every project |
| `projects/<name>/project.toml` | id prefix and git remotes |
| `projects/<name>/state.md` | current state |
| `projects/<name>/gotchas.md` | project-specific pitfalls and rules |
| `projects/<name>/facts.md`, `profile/facts.md` | values with the dates they held |
| `projects/<name>/events.md` | what happened once or is planned |
| `proposed-rules.md` (in `profile/` and each project) | rules the sweep proposed; not in effect |
| `projects/<name>/items/<id>-<slug>.md` | bugs, features, ideas, debt |
| `projects/<name>/decisions/<id>-<slug>.md` | decisions |
| `projects/<name>/sessions/<date>-<time>-<session8>.md` | handoff cards |
| `notes/` | knowledge that belongs to no single project |
| `.reprise/` | settings and machine-local cache; do not edit by hand |

## Records

Ids: items `<prefix>-0001`, decisions `<prefix>-D-0001`, numbered after the highest existing one; the prefix
is in `project.toml`. File names are the id and the title as an ASCII slug of at most 40 characters.
Dates are `YYYY-MM-DD`, times `YYYY-MM-DD HH:MM`. Field names, status values and headings stay exactly as
below; write the content in the user's language.

Item:

```markdown
---
id: api-0031
title: 401 after login
type: bug                 # bug | feature | idea | debt
status: inbox             # inbox | planned | doing | done-unverified | verified | dropped
created: 2026-10-24
updated: 2026-10-24
aliases: [auth error]     # extra search terms
files: [src/auth/session.ts]
commit:                   # the commit that did the work
verify:                   # a command that proves it works
verified_at:
related: [api-D-0004]
captured_during:          # what was going on when it came up
---

## Problem

## Cause

## Solution

## Verification

## Notes
```

Decision:

```markdown
---
id: api-D-0004
title: Keep the session in a cookie
status: accepted          # accepted | superseded
superseded_by:
created: 2026-10-24
files: [src/auth/]
commit:                   # HEAD when the decision was taken
related: [api-0031]
---

## Context

## Decision

## Why

## Alternatives
```

Handoff card:

```markdown
---
session: 1a2b3c4d
project: api
date: 2026-10-24 14:30
source: handoff
---

## Done

## Decisions

## Open

## Next step

## Touched records
```
