# reprise vault

Long-term memory for coding agents, kept by reprise. Every file is plain Markdown or TOML; you can read and
edit them by hand.

| Path | What |
| --- | --- |
| `profile/user.md` | who you are and how you work; read at every session start |
| `profile/rules.md` | rules that apply to every project |
| `projects/<name>/state.md` | what the project is and where it stands |
| `projects/<name>/items/` | bugs, features, ideas, debt |
| `projects/<name>/decisions/` | decisions and why they were taken |
| `projects/<name>/sessions/` | handoff cards between sessions |
| `projects/<name>/gotchas.md` | project-specific pitfalls |
| `notes/` | knowledge that belongs to no single project |

Start by filling in `profile/user.md`. Projects appear on their own the first time an agent works in a
repository.

With git installed, reprise keeps the vault's history: it commits what it writes and never pushes. Add a remote
and push to back it up or sync it between machines; `.reprise/cache/` is machine-local and ignored.
