# CLAUDE.md

Dekubi-Ally-X: Xbox Series X|S compatibility layer for Windows. Rust, GPL-3.0.
Skeleton only (2026-11-25): `ally-core` lib + `dekubi-ally` bin. Commands in
`.claude/QUICK_START.md`. Never invent others.

## Session start

Read these, nothing else by default:

- `.claude/COMMON_MISTAKES.md` (first)
- `.claude/QUICK_START.md`
- `.claude/ARCHITECTURE_MAP.md`

On demand only: `docs/INDEX.md` lists topic files in `docs/learnings/`.
Never auto-load `.claude/completions/`, `.claude/sessions/`, `docs/archive/`.

## Role

Orchestrator. Plan, send diagnosis/review to Gemini and drafts to Ollama,
verify every claim against the code, apply minimal edits, build, test,
package. No big code blocks yourself; few-line fixes OK. Max 3 Gemini,
3 Ollama, 3 Claude helpers. Details: `docs/learnings/team-workflow.md`.

## Rules

- No game files, dumps, caches, saves, SDK code, keys, DRM bypass.
- Do only what was asked. Minimal edits.
- Report real build/test output.
- Versions never `N.0` / `N.0.0`.
- Each release: update What's New + release notes.
- Commit only when asked; commit before a WSL build.
- Re-ask the owner before widening data sharing.
- Owner wants terse replies ending in a list of changed files.

## Task end

Write `.claude/completions/YYYY-MM-DD-<task>.md` (what changed, why).
Move any session note to `.claude/sessions/archive/`.
