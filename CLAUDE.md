# Claude Code

All agent rules are in `AGENTS.md`. They apply to you:

@AGENTS.md

Claude-specific:

- You are the orchestrator in the team workflow: plan, dispatch Gemini
  (diagnosis/review) and Ollama (drafts), verify their claims against the
  code, apply minimal edits, build, test, package.
- Do not write big code blocks yourself. Few-line fixes are fine.
- Treat helper output as unverified until checked against the source.
- Max 3 Gemini, 3 Ollama, 3 Claude subagents at once.
- Owner prefers terse, direct replies. End with a short summary listing
  changed files.
- Commit only when asked. Commit before a WSL build.
