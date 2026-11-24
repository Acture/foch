# `eu4-analysis` (`foch-eu4-analysis`)

`eu4-analysis` is a read-only Python binary-analysis tool for Europa Universalis IV (Mach-O and Windows x86_64 PE executables). It extracts database loading rules, built-in script keyword catalogs, function evidence, and offline or Ghidra-backed script semantics without launching EU4 or mutating the game installation directory.

## Requirements & Installation

Defined in `pyproject.toml`:

- **Python**: `>=3.11,<3.14`
- **Dependencies**: `pyghidra==3.1.0`, `jpype1==1.5.2` (managed via `uv`)
- **Ghidra**: Required only for Ghidra-backed subcommands (`discover`, `inspect`, and non-offline `semantics`) via `--ghidra` or `GHIDRA_INSTALL_DIR`. Offline PE review (`semantics --offline`) and `builtins` run without Ghidra.

Run the CLI and unit test suite from the workspace root with `uv`:

```sh
# Run the unit test suite
uv run --locked --directory src/tools/eu4-analysis python -m unittest discover -s tests

# Invoke the CLI
uv run --locked --directory src/tools/eu4-analysis python -m eu4_analysis --help
```

## CLI Subcommands

All subcommands are registered in `eu4_analysis/__main__.py`. Output paths for `discover` and `semantics` are validated to ensure no files are written inside the EU4 installation directory.

| Subcommand | Mode | Required Flags | Optional Flags & Defaults | Description |
| :--- | :--- | :--- | :--- | :--- |
| `builtins` | Offline | `--output <PATH>` | *(none)* | Writes the candidate JSON catalog of reserved, contextual, and alias script keywords plus base scopes (`country`, `province`). |
| `discover` | Ghidra | `--binary <PATH>`<br>`--game-version <LABEL>` | `--ghidra <PATH>` (`$GHIDRA_INSTALL_DIR`)<br>`--workspace <PATH>` (`target/eu4-analysis`)<br>`--timeout <INT>` (`45`)<br>`--bootstrap <GLOB>` (`*CEU4Application*LoadDatabasesEv`)<br>`--rules-dir <PATH>` (`src/packages/foch/src/game/eu4/content/rules`)<br>`--registry <LABEL>`<br>`--record <LABEL>` *(requires `--registry`)*<br>`--limit <INT>` *(batch mode only; incompatible with `--registry`)* | Discovers database loader candidates from the bootstrap function and exports versioned loading rules or single-registry family evidence. |
| `inspect` | Ghidra | `--binary <PATH>`<br>`--game-version <LABEL>`<br>`--symbol <GLOB>` | `--ghidra <PATH>` (`$GHIDRA_INSTALL_DIR`)<br>`--workspace <PATH>` (`target/eu4-analysis`)<br>`--timeout <INT>` (`45`) | Matches a single function by symbol/address glob and exports its disassembled instructions, call sites, and decompiled pseudocode to `<workspace>/exports/<game_version>/<sha256>/<address>.json`. |
| `semantics` | Ghidra | `--binary <PATH>`<br>`--game-version <LABEL>`<br>`--catalog <PATH>`<br>`--output <PATH>` | `--ghidra <PATH>` (`$GHIDRA_INSTALL_DIR`)<br>`--workspace <PATH>` (`target/eu4-analysis`)<br>`--timeout <INT>` (`45`) | Locates effect and trigger registrations, decompiles handlers in Ghidra, records `pseudocode_sha256` pins, and writes a reviewable semantics catalog. |
| `semantics --offline` | Offline PE | `--binary <PATH>`<br>`--game-version <LABEL>`<br>`--catalog <PATH>`<br>`--output <PATH>`<br>`--offline` | `--workspace <PATH>` (`target/eu4-analysis`)<br>`--timeout <INT>` (`45`) | Performs read-only PE token-table discovery, 6-token gold calibration, console-command rejection, debug-string handler pinning, and fail-closed token-id dispatch without starting EU4 or Ghidra. |

> **Label & validation rules**: `--game-version`, `--registry`, and `--record` must match `[A-Za-z0-9][A-Za-z0-9_.-]*`. `--timeout` and `--limit` must be positive integers (`> 0`). Invalid arguments exit with `argparse` status code `2`.

### Example: Offline Semantics Review

```sh
# Offline PE semantics review (no Ghidra or EU4 launch required)
uv run --locked --directory src/tools/eu4-analysis python -m eu4_analysis semantics \
  --offline \
  --binary "C:/Program Files (x86)/Steam/steamapps/common/Europa Universalis IV/eu4.exe" \
  --game-version 1.37.5 \
  --catalog ../../packages/foch-test/src/interpret/semantics.json \
  --output ../../../target/eu4-analysis/semantics-review.json

# Builtin script keyword catalog generation
uv run --locked --directory src/tools/eu4-analysis python -m eu4_analysis builtins \
  --output ../../../target/eu4-analysis/builtins.json

# Ghidra-backed database loader discovery (batch or single-registry probe)
uv run --locked --directory src/tools/eu4-analysis python -m eu4_analysis discover \
  --binary "C:/Program Files (x86)/Steam/steamapps/common/Europa Universalis IV/eu4.exe" \
  --game-version 1.37.5 \
  --registry CBuildingDataBase \
  --record CBuilding
```

## Offline PE Token Table & Handler Pinning (`semantics --offline`)

On stripped Windows x86_64 PE executables, `semantics --offline` combines exact `.pdata` unwind function bounds (`eu4_analysis/pe.py`) with structural token-table discovery and debug-string cross-referencing (`eu4_analysis/tokens.py`, `eu4_analysis/semantics.py`).

### 1. Fixed-Stride 16-Bit Token Table Auto-Discovery

`discover_token_table` locates the Paradox lexer's token table across non-`.text` PE sections without hardcoded file offsets or RVAs:

- Scans non-code sections for NUL-terminated ASCII identifier strings (`[A-Za-z_][A-Za-z0-9_]*`, maximum length `64` bytes).
- Evaluates candidate record strides in `0x10..=0x400` with at least `8` aligned entries and checks 2-byte (`u16`) and 4-byte (`u32`) integer fields immediately preceding each identifier string.
- Requires at least `90%` of aligned entries to carry a valid 16-bit token ID (`0 < id < 0x10000`) and verifies density by checking that nearby entries (`1 <= steps <= 8` stride slots apart) have token IDs differing by `steps` in at least `50%` of adjacent pairs.

### 2. Six-Token Cross-Repo Gold Calibration

Before trusting any discovered token ID, `calibrate` verifies that all six cross-repo gold tokens in `GOLD_TOKENS` (`eu4_analysis/tokens.py`) resolve to their exact expected 16-bit IDs:

| Token Name | Expected Hex ID |
| :--- | :--- |
| `outgoing` | `0x2AA8` |
| `members` | `0x2AA9` |
| `army` | `0x28A0` |
| `regiment` | `0x28A2` |
| `strength` | `0x28A9` |
| `morale` | `0x28A8` |

If calibration does not match `6/6`, token IDs are treated as untrusted (`token_table_absent` or `token_table_uncalibrated`).

### 3. Console-Command Rejection (`+0x30` vs. `+0x40`)

When scanning static registration records that reference a keyword string pointer (`string_sites` and `baseline_observation` in `eu4_analysis/pe.py`):

- Both script handler registrations and console-command registrations can store a `.pdata`-backed `.text` function pointer at offset `+0x30` (six `qword`s after the keyword string pointer).
- Console-command records additionally store a pointer at offset `+0x40` to an ASCII argument-hint string beginning with `<` (such as `<Flag>` or `<Amount>`).
- Any record carrying a `<...>` argument hint at `+0x40` is classified as a console command (`("console", va)`) and excluded from `table_handlers`. If a keyword only matches console-command records, baseline string scanning reports `rejected_console_cmd` rather than falsely pinning the console wrapper as a script effect or trigger handler.

### 4. Strict Evidence Boundary

`review_catalog` classifies each catalog primitive into one of four treatment statuses:

- **`pinned`**: A debug-string Execute/Evaluate function, or a token-id dispatcher that names one `.pdata` function for that token. Debug strings win when both exist: `debug_handlers` finds a unique `.pdata` function in `.text` containing a 7-byte RIP-relative `LEA` to a non-`.text` C-string matching `C([A-Z][A-Za-z0-9]*)(Effect|Trigger)::(Execute|Evaluate)\b`, the snake-cased class stem matches the primitive name, and the method matches its kind (`Effect::Execute` for `effect`, `Trigger::Evaluate` for `trigger`). The handler record carries `address`, `end`, `rva`, `debug_string`, `debug_string_rva`, `lea_rva`, and `prologue`. A jump table or learned vtable slot uses `evidence` `token_jump_table` or `token_vtable` and the same bounds fields without a debug string.
- **`factory_pinned`**: The calibrated token appears once in a repeated `(token id, function pointer)` run, and that `.pdata` function is not shared with another token in the run and is not a console-command handler for the keyword. `evidence` is `token_factory_pair`.
- **`token_only`**: The calibrated token table contains a unique 16-bit `token_id`, but no handler was proven. `failure` is `handler_unproven`, `handler_ambiguous`, `kind_mismatch`, `dispatcher_ambiguous`, `dispatcher_unverified`, or `dispatcher_console`.
- **`unpinned`**: No unique handler is proven and no unique calibrated token id exists (`token_absent`, `token_ambiguous`, `token_table_absent`, or `token_table_uncalibrated`).

### 5. Token-Id Dispatch

After calibration, `eu4_analysis/dispatch.py` looks for three shapes. None of them uses a hardcoded file offset or RVA. A shape that does not prove one function per token is a refusal, not a guess.

- **RIP-relative jump table.** `sub r32, imm32` / `cmp` / `ja` followed by a RIP-relative `lea` of a dword table, a scaled load, `add` of that table base, and `jmp`. The immediate must be a calibrated token id. Slot `index` is token `base + index`, and the stored dword is relative to the table. Every in-range slot must be a `.pdata` function start outside the dispatcher. Two tokens that land on one function stay `dispatcher_ambiguous`. A slot that is not a function start refuses the whole table (`dispatcher_unverified`). A table addressed through an unproven register (the `r15 + disp` form) is not read.
- **Factory pairs.** In a non-code section outside the token-name table, a run of at least eight records with one discovered stride, a `u32` token id, and a `.pdata` function pointer. A function used by more than one token in the run is `dispatcher_ambiguous`.
- **Vtable slot.** A `.pdata` function whose body is `mov eax, token; ret` (padding may be `nop` or `int3`). Its distance to an Execute/Evaluate pointer is learned only when at least two debug-string handlers share that distance and one interior slot. Other thunks with the same interior slot resolve to that Execute/Evaluate pointer. One function claimed by two tokens is `dispatcher_ambiguous`.

A resolved address that is already a console-command handler for that keyword (`<...>` hint at `+0x40`) is `dispatcher_console` and stays `token_only`.

Each `semantics-pin-review` JSON artifact (`format: 1`, `kind: "semantics-pin-review"`) records:

| Top-Level Key | Purpose |
| :--- | :--- |
| `binary_sha256` / `pinned_binary_sha256` | SHA-256 of the analyzed PE executable and whether it matches `PINNED_EU4_SHA256` (`pinned_binary_match`). |
| `token_table` | Auto-discovered PE section (`section`), byte `stride`, `id_width`, `records` count, `first_rva`, and `last_rva`. |
| `calibration` | Pass status (`passed`, `matched`, `total`) and per-token `checks` (`name`, `expected`, `found`, `match`). |
| `comparison` | Aggregate counts across `baseline` (`pinned`, `rejected_console_cmd`, `no_handler`, `ambiguous`, `string_absent`), `treatment` (`pinned`, `factory_pinned`, `token_only`, `unpinned`), and `failures`. |
| `primitives` | Per-primitive map containing `kind`, `baseline`, `console_handlers`, `treatment`, `failure`, `token_id`, and `handler`. |

## Known Limits & Verification Boundary

Against the pinned EU4 `1.37.5` Windows x64 executable (`PINNED_EU4_SHA256 = "c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e"`), `semantics --offline` produces the following verified baseline and treatment breakdown across the 70 `foch-test` primitives:

- **Gold calibration**: Passes `6/6` (`outgoing`, `members`, `army`, `regiment`, `strength`, `morale`).
- **`pinned` (`3/70`)**: `set_country_flag`, `set_variable`, and `change_variable` are pinned to exact `.pdata` function bounds via unique `C<Name>Effect::Execute` debug-string references. Baseline registration-table scanning rejects `set_country_flag` as `rejected_console_cmd`; its console handler (`0x140e91d10`) is not the pinned Execute function (`0x140611870`).
- **`factory_pinned` (`0/70`)**: No catalog token has a unique factory-pair function that is not a console handler.
- **`token_only` (`66/70`)**: The other primitives that are present in the token table stay `token_only` with `failure: "handler_unproven"`. On this executable the provable dispatch shapes do not name a function for them. Execute pointers for the debug-string handlers sit in packed `.rdata` vtables, but those vtable addresses are not RIP-relative LEA targets tied to the token, and the Execute RVAs are not jump-table dwords (their only dword hits are `.pdata`). Token immediates in `.text` feed shared readers: an equal-arm calls one shared function with a type code instead of jumping to a per-token function start. The repeated `(token, function pointer)` runs that do exist are GUI widget tables, and one function serves many tokens, so they are not factories. A few `mov eax, token; ret` sequences are not `.pdata` functions, so they are not vtable anchors.
- **`unpinned` (`1/70`)**: `has_variable` is absent from the EU4 `1.37.5` PE token table and remains `unpinned` with `failure: "token_absent"`.
- **Full handler decompilation**: Offline PE review recovers token IDs, console-command rejections, and debug-string handler bounds with decoded prologues, but does not fabricate decompiled pseudocode hashes (`pseudocode_sha256`). Full handler decompilation across unstripped or dispatcher-routed primitives still requires a Ghidra session (`semantics` without `--offline`) or a symbol-bearing Mach-O binary.
