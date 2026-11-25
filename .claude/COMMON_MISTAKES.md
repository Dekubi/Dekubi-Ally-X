# Common mistakes

Read first each session.

1. **Committing game data.** `.rpf`, `.xvd`, `.ttpf`, dumps, caches, saves.
   Check `git status` before every commit. Game folders stay outside the repo.
2. **Pasting XCL code.** xcl-dev/xcl is GPL-2.0-only, incompatible with this
   GPL-3.0 repo. Reference only; reimplement. VIctory family is GPL-3.0+:
   reuse OK, keep headers, credit in commit body and README.
3. **Trusting Ollama drafts.** qwen2.5-coder:7b output is buggy and invents
   release-note sections. Verify every line against the code.
4. **Letting Gemini act.** Always `--mode plan`, prompt says "Do NOT run shell
   commands and do NOT edit files", never `--dangerously-skip-permissions`.
5. **Bad version strings.** `N.0` / `N.0.0` break the updater.
