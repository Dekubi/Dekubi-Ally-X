# Team workflow (owner's Windows PC)

Roles:

| Agent | Role | Never |
|-------|------|-------|
| Gemini (`agy` CLI) | Read-only diagnosis, bug hunting, log reading, review after fixes | Run shell commands, edit files |
| Ollama `qwen2.5-coder:7b` | Drafts code, patches, release notes | Final say; drafts are unverified |
| Claude | Plans, verifies helper claims against code, applies minimal edits, builds, tests, packages | Write large code blocks itself (few-line fixes OK) |

Limits: max 3 Gemini, 3 Ollama, 3 Claude helpers at once.

Pipeline:

1. Plan
2. Gemini diagnoses
3. Ollama drafts (one file per draft, parallel)
4. Claude verifies and applies
5. Compile
6. Gemini reviews
7. Fix
8. Build + test
9. Package
10. Short summary listing changed files

## Gemini

- Binary: `C:\Users\User\AppData\Local\agy\bin\agy.exe`
- Launcher: `C:\Users\User\claude_notes\launch_gemini.ps1`
- Always `--mode plan`. Prompt via stdin redirect. Report to a file in
  `C:\Users\User\claude_notes`.
- Every prompt includes: "Do NOT run shell commands and do NOT edit files."
- Never `--dangerously-skip-permissions`.
- agy-fast plugin: allow only `batch_search`, `read_slices`, `quick_outline`.

## Ollama

- Model `qwen2.5-coder:7b` at `localhost:11434`.
- `python C:\Users\User\claude_notes\ollama_draft.py <task.txt> [ctx...] --out <ans.md>`
- Prompts under 45K chars.
- Drafts are buggy and invent sections in release notes. Verify every line.

## Habits

- Commit before a WSL build.
- `MSYS_NO_PATHCONV=1` when calling WSL from Git Bash.
- Long builds in the background.
- Keep the notes overlay in `C:\Users\User\claude_notes` current.

