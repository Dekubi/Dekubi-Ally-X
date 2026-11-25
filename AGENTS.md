# Instructions for AI agents

For any coding agent (Claude Code, Gemini, Codex, Copilot, Cursor, local
models) working in this repository. Claude Code starts from `CLAUDE.md`.

## Project

Dekubi-Ally-X: Xbox Series X|S compatibility layer for Windows. Title x86-64
code runs natively; we supply the console platform and translate GPU work to
D3D12 / DXIL. See `README.md`.

State (2026-11-25): workspace skeleton only (`crates/ally-core`,
`apps/dekubi-ally`). Layout: `.claude/ARCHITECTURE_MAP.md`.

## Build and test

Windows 10/11 x64, toolchain pinned in `rust-toolchain.toml`.

```powershell
cargo build --workspace
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

CI (`.github/workflows/ci.yml`) runs the same plus an asset-policy check that
fails on tracked game data, binaries or archives.

## Hard rules

- Never commit game files, dumps, caches, saves, console SDK code/headers/
  binaries, or keys. No code that decrypts packages or circumvents DRM.
- Do what was asked, nothing more. No unrequested refactors, renames,
  reformatting or dependency changes.
- Minimal edits. Follow surrounding code.
- Report real build/test output. Never claim "should pass".
- Windows-only target. On Linux/macOS say "not built / not tested".
- Version strings: never `N.0` or `N.0.0` (breaks the updater).
- Every release: update What's New and the release notes.
- Credit Fritchii and Lichii for features in release notes / What's New.
- Re-ask the owner before any change that widens data sharing.

## More, on demand

Read only when the task needs it. Index: `docs/INDEX.md`.

- `docs/learnings/team-workflow.md`: Gemini/Ollama/Claude roles, pipeline,
  commands, limits.
- `docs/learnings/reference-repos.md`: reusing VIctory/victory-fork/
  VItoriaBr/ztrx code, XCL license limit.
- `docs/learnings/perf-glossary.md`: owner's performance terms.
- `.claude/COMMON_MISTAKES.md`: top mistakes; read before any change.
