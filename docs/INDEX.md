# Docs index

## Session start (always)

- `CLAUDE.md`
- `.claude/COMMON_MISTAKES.md`
- `.claude/QUICK_START.md`
- `.claude/ARCHITECTURE_MAP.md`

## On demand

| File | Load when |
|------|-----------|
| `docs/learnings/team-workflow.md` | Dispatching Gemini/Ollama, running the pipeline |
| `docs/learnings/reference-repos.md` | Porting code from another project |
| `docs/learnings/perf-glossary.md` | Perf work, profiling, owner uses a perf term |

## Never auto-load

`.claude/completions/`, `.claude/sessions/`, `docs/archive/`.

Structure adapted from
[claude-token-optimizer](https://github.com/nadimtuhin/claude-token-optimizer)
(MIT, Nadim Tuhin).
