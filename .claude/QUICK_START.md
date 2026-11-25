# Quick start

## Build / test

```powershell
cargo build --workspace                 # debug
cargo build --release -p dekubi-ally    # release
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Toolchain pinned in `rust-toolchain.toml` (1.98.1, msvc). CI runs fmt,
clippy, tests on `windows-latest` plus an asset-policy check.

## Helpers (owner's PC)

```powershell
# Gemini, read-only, report to claude_notes
C:\Users\User\claude_notes\launch_gemini.ps1

# Ollama draft (prompt < 45K chars)
python C:\Users\User\claude_notes\ollama_draft.py <task.txt> [ctx...] --out <ans.md>
```

WSL from Git Bash: prefix `MSYS_NO_PATHCONV=1`. Long builds in background.
