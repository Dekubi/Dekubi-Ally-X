# Architecture map

## Repo

| Path | What |
|------|------|
| `CLAUDE.md`, `.claude/` | Claude session docs |
| `AGENTS.md` | Rules for any agent |
| `docs/INDEX.md`, `docs/learnings/` | Topic docs, load on demand |
| `README.md` | Project description, credits |
| `Cargo.toml` | Workspace: `crates/*`, `apps/*` |
| `crates/ally-core/` | Shared types and helpers (lib) |
| `apps/dekubi-ally/` | Command-line front end (bin) |
| `.github/workflows/ci.yml` | Asset policy, fmt, clippy, tests |
| `.github/` | PR/issue templates, dependabot |

New subsystems go in `crates/ally-<name>/` (loader, hle, gpu, shader,
formats, ...), mirroring VIctory's split.

## Reference clones (owner's PC)

`C:\Users\User\Dekubi Box Project\`: `VIctory`, `victory-fork`,
`VItoriaBr`, `ztrx`, `xcl`. See `docs/learnings/reference-repos.md`.
