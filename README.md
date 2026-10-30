# reprise

A second brain for your coding agent. Each session starts knowing the project's state, open work, past
decisions and your rules; each session ends by writing down what changed. Everything lives in a vault: plain
Markdown in a folder you own. Works with Claude Code on macOS and Linux.

## Install

```bash
brew install emredagci/tap/reprise
```

Or from source:

```bash
cargo install --locked --git https://github.com/emredagci/reprise reprise
```

## Use

```bash
reprise setup set --vault <folder> --auto
```

```bash
reprise connect claude --yes
```

```bash
reprise sweep schedule
```

The first picks the vault (`--auto` lets reprise read every folder; `--folder <path>` limits it). The second gives
Claude Code its hooks. The third distills sessions that ended without a handoff, in the background.

`reprise status` shows what is set up and what needs you; `reprise doctor` checks the vault; `reprise --help`
lists every command. Details: [docs/guide.md](docs/guide.md).

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
