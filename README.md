# sbrain

A second brain for your coding agent. Each session starts knowing the project's state, open work, past
decisions and your rules; each session ends by writing down what changed. Everything lives in a vault: plain
Markdown in a folder you own. Works with Claude Code on macOS and Linux.

## Install

```bash
brew install emredagci/tap/sbrain
```

Or from source:

```bash
cargo install --locked --git https://github.com/emredagci/sbrain sbrain
```

## Use

```bash
sbrain setup set --vault <folder> --auto
```

```bash
sbrain connect claude --yes
```

```bash
sbrain sweep schedule
```

The first picks the vault (`--auto` lets sbrain read every folder; `--folder <path>` limits it). The second gives
Claude Code its hooks. The third distills sessions that ended without a handoff, in the background.

`sbrain status` shows what is set up and what needs you; `sbrain doctor` checks the vault; `sbrain --help`
lists every command. Details: [docs/guide.md](docs/guide.md).

## Build

```bash
cargo build --release -p sbrain
```

```bash
cargo test --workspace
```

The Rust version is pinned in `rust-toolchain.toml`; rustup installs it (on macOS: `brew install rustup`, then
put `/opt/homebrew/opt/rustup/bin` on PATH). The binary is `target/release/sbrain`.

## License

Apache-2.0 ([LICENSE](LICENSE), [NOTICE](NOTICE)). Not affiliated with Anthropic or OpenAI.
