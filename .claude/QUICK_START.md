# Quick start

## Build / test

None yet. Record real `cargo` commands here when the workspace lands.

## Helpers (owner's PC)

```powershell
# Gemini, read-only, report to claude_notes
C:\Users\User\claude_notes\launch_gemini.ps1

# Ollama draft (prompt < 45K chars)
python C:\Users\User\claude_notes\ollama_draft.py <task.txt> [ctx...] --out <ans.md>
```

WSL from Git Bash: prefix `MSYS_NO_PATHCONV=1`. Long builds in background.
