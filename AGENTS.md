# Instructions for AI agents

For any coding agent (Claude Code, Gemini, Codex, Copilot, Cursor, local
models) working in this repository. Claude-specific notes are in `CLAUDE.md`.

## Project

Dekubi-Ally-X: Xbox Series X|S compatibility layer for Windows. Title x86-64
code runs natively; we supply the console platform and translate GPU work to
D3D12 / DXIL. See `README.md`.

State (2026-11-25): pre-code. Only `LICENSE` (GPL-3.0), `.gitignore`
(Rust/Cargo) and docs exist. Do not invent build or test commands; when the
Cargo workspace lands, record the real ones here.

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

## Reusing reference code

Owner has permission from the authors to reuse code from:
Marvisak/VIctory, fxceless/victory-fork, uiliannn/VItoriaBr, Zettrox/ztrx
(all GPL-3.0-or-later) and xcl-dev/xcl (GPL-2.0-only).

- Keep each file's copyright headers and license notices.
- Credit the source in the commit body and in `README.md` Credits.
- XCL is GPL-2.0-only: incompatible with GPL-3.0. Use as reference;
  reimplement, do not paste, unless its authors relicense that code.
- Owner keeps local clones next to this repo in
  `C:\Users\User\Dekubi Box Project\` (`VIctory`, `victory-fork`,
  `VItoriaBr`, `xcl`, `ztrx`).

## Team workflow (owner's Windows PC)

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

### Gemini

- Binary: `C:\Users\User\AppData\Local\agy\bin\agy.exe`
- Launcher: `C:\Users\User\claude_notes\launch_gemini.ps1`
- Always `--mode plan`. Prompt via stdin redirect. Report to a file in
  `C:\Users\User\claude_notes`.
- Every prompt includes: "Do NOT run shell commands and do NOT edit files."
- Never `--dangerously-skip-permissions`.
- agy-fast plugin: allow only `batch_search`, `read_slices`, `quick_outline`.

### Ollama

- Model `qwen2.5-coder:7b` at `localhost:11434`.
- `python C:\Users\User\claude_notes\ollama_draft.py <task.txt> [ctx...] --out <ans.md>`
- Prompts under 45K chars.
- Drafts are buggy and invent sections in release notes. Verify every line.

### Habits

- Commit before a WSL build.
- `MSYS_NO_PATHCONV=1` when calling WSL from Git Bash.
- Long builds in the background.
- Keep the notes overlay in `C:\Users\User\claude_notes` current.

## Performance vocabulary

Terms the owner uses. Use them precisely.

- **GPU:** overdraw, GPU instancing, static/dynamic batching, texture
  atlasing, mipmaps, backface culling, occlusion culling, baked lighting,
  dynamic resolution / upscaling, Nanite (virtualized geometry).
- **CPU / memory:** spatial partitioning, data-oriented design, data
  locality, SIMD, loop unrolling, inlining, custom allocators,
  allocation-free design, amortization, dirty flag, memoization,
  bit-twiddling.
- **Profiling:** hotspot, bottleneck, CPU-bound, GPU-bound, memory-bound,
  self time, frame pacing, I/O bottleneck, VRAM, asset streaming, frame
  rate cap.
