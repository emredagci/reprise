# reprise

[![crates.io](https://img.shields.io/crates/v/reprise.svg)](https://crates.io/crates/reprise)
[![CI](https://github.com/emredagci/reprise/actions/workflows/ci.yml/badge.svg)](https://github.com/emredagci/reprise/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/reprise.svg)](LICENSE)

A second brain for your coding agent.

Every session starts knowing the project's state, open work, past decisions and your rules. Every session ends
by writing down what changed. When a session ends without that, reprise reads its transcript later and writes it
down for you. Everything lives in a vault: plain Markdown in a folder you own, kept in git.

Works with Claude Code on macOS and Linux. Single binary, no server, no account.

## What it does

- **Context at session start.** A hook gives the agent your profile and rules, the project's state, pitfalls,
  open work, current facts and recent events, and tells it how to keep the vault.
- **Recall on every prompt.** The records related to what you asked come along with it.
- **Records, not logs.** Items (bugs, features, ideas), decisions with their reasons, rules, handoff cards, dated
  facts and events, one Markdown file or table row each.
- **Sweep.** Sessions that ended without a handoff are distilled in the background by your coding CLI
  (Claude Code or Codex) or OpenRouter.
- **Evidence or nothing.** A distilled record must quote the session word for word. A rule may only quote you.
  Records without a quote, or that the engine is unsure of, are left out and listed.
- **Secrets stay out.** Keys, tokens and passwords are masked before anything reaches the vault.
- **Compaction-proof.** When Claude Code compacts a session, your instructions come back with every prompt
  after it.
- **Time aware.** "Yesterday" and "last week" are turned into dates. Facts close when they change. The timeline
  is computed, not guessed.
- **Health check.** `reprise doctor` finds broken records and links, secrets, forgotten or unverified work, and
  backup problems.

## Install

```bash
brew install emredagci/tap/reprise
```

Or with Cargo, from [crates.io](https://crates.io/crates/reprise) (builds from source; needs Rust 1.90 or later):

```bash
cargo install --locked reprise
```

Archives for macOS and Linux are on the [releases page](https://github.com/emredagci/reprise/releases/latest).

## Get started

```bash
reprise setup set --vault <folder> --auto
```

```bash
reprise connect claude --yes
```

```bash
reprise sweep schedule
```

1. Picks the vault. An empty folder becomes a new vault. `--auto` lets reprise read every folder;
   `--folder <path>` limits it.
2. Gives Claude Code its hooks and permission to run `reprise`.
3. Runs the sweep in the background (launchd on macOS, a systemd timer on Linux).

`reprise status` then shows what is set up and what needs you.

## Commands

| Command | What it does |
| --- | --- |
| `reprise status` | Vault, connections, engine, sweep and what needs you, at one glance |
| `reprise doctor` | Checks the vault; changes nothing |
| `reprise overview` | Counts, open items and latest decisions per project |
| `reprise search <query>...` | Searches records, follows links one step; `--semantic` searches by meaning |
| `reprise fact set\|end\|list` | Values that hold for a while: a version, a port, a host |
| `reprise event add\|list`, `reprise timeline` | What happened or is planned, in order, with gaps and counts |
| `reprise rule list\|accept\|reject` | Rules the sweep proposed; none is in effect until you accept it |
| `reprise engine set` | The CLI and model that distill sessions |
| `reprise reset` | Session start leaves out a project's past from a day on; `--undo` takes it back |
| `reprise export`, `reprise restore` | Moves a vault as one file |

`reprise --help` lists every command. How it works, configuration and the sweep:
[docs/guide.md](docs/guide.md). The vault layout and record fields: [docs/vault-format.md](docs/vault-format.md).

## The vault

```
<vault>/
  AGENTS.md            how an agent keeps this vault
  profile/             who you are, your rules, facts about no single project
  notes/               knowledge that belongs to no single project
  projects/<project>/
    state.md           current state, kept short
    gotchas.md         project pitfalls and rules
    facts.md           values with the dates they held
    events.md          what happened, in order
    items/             bugs, features, ideas, debt
    decisions/         one file per decision
    sessions/          handoff cards
```

reprise commits what it writes, one commit each, and never pushes. Add a remote to back the vault up or share it
between machines.

## Remove

```bash
reprise uninstall --yes
```

```bash
brew uninstall reprise
```

The first removes the connections, the scheduled sweep and reprise's settings; the vault stays. Without
`--yes` it only lists what it would remove.

## Build

```bash
cargo build --release -p reprise
```

```bash
cargo test --workspace
```

The Rust version is pinned in `rust-toolchain.toml`; rustup installs it (on macOS: `brew install rustup`, then
put `/opt/homebrew/opt/rustup/bin` on PATH). The binary is `target/release/reprise`.

## License

Apache-2.0 ([LICENSE](LICENSE), [NOTICE](NOTICE)). Not affiliated with Anthropic or OpenAI.
