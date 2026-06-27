# Merge-quality harness

Measures foch's structural-merge quality against community-authored **compatibility
patches** ("compatches") — these are the human-written ground truth for "what a good
merge of mod A + mod B looks like".

## What it does

1. **discover** — Steam Web API `QueryFiles` → every EU4 "Compatch" workshop item.
2. **resolve-pairs** — `GetPublishedFileDetails` → regex each description for the patched
   mod IDs (compatches embed the two mods they patch as workshop URLs). Cached to
   `corpus.json`; network only happens here.
3. **filter-local** — keep cases where the compatch **and** all patched mods exist in your
   local Steam workshop dir. (Mod files cannot be downloaded via the API — Steam enforces
   ownership — so only locally-subscribed cases are testable.)
4. **run** — synthesise a 2-mod playset `{modA, modB}` and run `foch merge`.
5. **score** — for every file the compatch hand-merged, classify foch's output
   structurally (top-level definitions) **and** semantically (normalised-text similarity
   vs the compatch), then aggregate.

### Per-file verdicts
- `matches_human` — foch's merge ≈ the hand-written compatch (same defs, ≥0.92 similar).
- `diverges_formatting` — same definitions, different text/formatting.
- `diverges_structure` — different set of top-level definitions vs the human.
- `drops_content` — foch lost a top-level def present in mod A or B (load-order failure mode).
- `conflict_withheld` — foch surfaced a manual conflict; the human resolved it by hand.
- `not_emitted` — foch produced no file for this path.

## Setup

- Put your Steam Web API key in the repo-root `.env` (gitignored): `STEAM_API_KEY=...`
  (get one at https://steamcommunity.com/dev/apikey).
- Build foch: `cargo build --release -p foch-cli` (the harness uses `target/release/foch.exe`).
- Install an EU4 base-data snapshot first: `foch data install eu4 --game-version auto` (or
  `foch data build eu4 --from-game-path <game> --game-version auto --install`).

## Run

```bash
python scripts/merge_quality/merge_quality.py all          # discover + run + report
python scripts/merge_quality/merge_quality.py discover     # refresh corpus.json only
python scripts/merge_quality/merge_quality.py run --limit 3 # score first 3 local cases
```

Env / flags: `STEAM_WORKSHOP_DIR` (default `G:\SteamLibrary\steamapps\workshop\content\236850`),
`FOCH_BIN`, `--corpus`, `--results-dir`, `--keep` (retain temp merge dirs).

Output: `scripts/merge_quality/results/report.md` + `results.json` (both gitignored).
