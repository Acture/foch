# `eu4-decide` (`foch-eu4-decide` Python Tooling)

Offline policy training, overhaul-mod category ablation, and JSON weight-export tooling for the EU4 decision pipeline.

This package (`src/tools/eu4-decide`, Python `>=3.11,<3.14`) pairs with three Rust crates in the workspace:
- **`foch-eu4-decide`** (`src/packages/foch-eu4-decide`): Pure-Rust decision inference, pocket-war `env_server` simulator, and `ResidualCategory` forward pass.
- **`foch-eu4-state`** (`src/packages/foch-eu4-state`): 29-column province feature plane (`NODE = 29`, `RULES = 26`) and digest-gated live memory readers over `foch_runtime::dataplane`.
- **`foch-plugin-read-decide`** (`plugins/eu4-read-decide`): Read-only in-game suggestion plugin (`dev.foch.read-decide`) that scores state frames without writing game memory.

---

## Environment & Commands

All Python commands run through `uv` from the workspace root using the locked environment in `src/tools/eu4-decide`:

```sh
# Run the Python unit test suite (ResidualCategory & 5-seed overhaul ablation)
uv run --locked --directory src/tools/eu4-decide python -m unittest discover -s tests -v

# Run the standalone 5-seed symmetric ablation and write the Rust parity fixture
uv run --locked --directory src/tools/eu4-decide python -m eu4_decide._ablation

# Train the trajectory GRPO linear candidate scorer against the Rust env_server
uv run --locked --directory src/tools/eu4-decide python -m eu4_decide \
  --scenarios 32 --rollouts 8 --rounds 12 --epochs 2 --weights target/decide-weights.json

# Run the zero-shot instruct-LLM baseline over env_server text descriptions
uv run --locked --directory src/tools/eu4-decide python -m eu4_decide \
  --llm --env cargo run -q -p foch-eu4-decide --example env_server -- \
  --model Qwen/Qwen2.5-0.5B-Instruct --scenarios 24 --rollouts 8
```

### CLI Flags (`python -m eu4_decide`)

- **Default mode (`eu4_decide._grpo`)**:
  - `--env <CMD...>`: Environment server command (default: `cargo run -q -p foch-eu4-decide --example env_server --`).
  - `--scenarios <INT>`: Scenarios per round (default: `32`).
  - `--rollouts <INT>`: Rollouts per scenario group (default: `8`).
  - `--rounds <INT>`: GRPO training rounds (default: `12`).
  - `--epochs <INT>`: Policy gradient epochs per batch (default: `2`).
  - `--weights <PATH>`: Output JSON path for scorer weights (default: `target/decide-weights.json`).
- **Zero-shot LLM mode (`--llm`, `eu4_decide._llm`)**:
  - `--llm`: Enable zero-shot causal-LM scoring instead of GRPO training.
  - `--env <CMD...>`: Required environment server command.
  - `--model <HF_ID>`: Hugging Face model ID (default: `Qwen/Qwen2.5-0.5B-Instruct`).
  - `--scenarios <INT>`: Number of scenarios to evaluate (default: `24`).
  - `--rollouts <INT>`: Rollouts per scenario (default: `8`).

### Paired Rust Verification

Verify the Rust inference crate, feature-plane state crate, and read-only plugin (including Python-to-Rust fixture parity):

```sh
cargo test -p foch-eu4-decide -p foch-eu4-state -p foch-plugin-read-decide
```

---

## Overhaul-Mod Generalization via `ResidualCategory` (`f(modifiers) + E_id`)

### Why Vanilla One-Hot IDs Break on Overhaul Mods

Policies that encode game categories (terrains, casus belli) as one-hot lookup tables over vanilla names fail under total-conversion and overhaul mods in two ways:
1. **Rewritten vanilla stats**: Mods keep vanilla identifiers (`plains`, `mountains`, `conquest`, `imperialism`) while editing or reversing their underlying modifiers. A pure ID lookup ignores the live numbers and ranks modified categories according to stale vanilla priors.
2. **Newly invented categories**: Mods add custom terrains (`haunted_bog`, `dwarven_hold`) and casus belli (`eldritch_crusade`, `dwarven_reclamation`) absent from the vanilla vocabulary, forcing one-hot baselines onto a fixed fallback (`plains` or `conquest`).

### Architecture (`eu4_decide._residual`)

`ResidualCategory` decomposes each category representation of width `W` into a continuous mechanic MLP plus a guarded identity residual:

$$\text{score}(\text{name}, x) = f(x) + E_{\text{resolve}(\text{name}, x)}$$

- **Continuous mechanic trunk `f(modifiers)`**: A 2-layer MLP (`Linear(5, W) -> ReLU -> Linear(W, W)`) over five mechanic axes (for terrain: `movement_cost`, `defence`, `combat_width`, `supply_limit`, `attrition`, with neutral defaults `1.0` on `movement_cost` and `supply_limit` and `0.0` elsewhere).
- **Reserved `<UNK>` at index `0`**: `CategoryTable` registers known vanilla names at indices `1..N` along with their exact training-time 5-element modifier vectors. Index `0` (`UNK = 0`) is `<UNK>`. Row `0` of `E_id` is zero-initialized, clamped by a backward hook (`_keep_unk_zero`), and masked to zero in `forward`.
- **Training-time ID dropout (`p in [0.30, 0.50]`)**: During training (`self.training == True`), each resolved ID is independently replaced with `0` (`<UNK>`) with probability `p` (`DROPOUT_MIN = 0.30`, `DROPOUT_MAX = 0.50`, default `0.40`), while the mechanic vector `x` is never dropped (`p = 0.0` is allowed only as the ablation control). Dropping IDs prevents `E_id` from memorizing the continuous mechanic signal and forces `f(x)` to learn the transferable mechanic mapping.
- **Inference-time resolution (`resolve` / `id_of`)**: At inference, ID dropout is disabled. `resolve(name, modifiers)` returns the registered index `1..N` **only** if `name` is in the table **and** its runtime mechanic vector matches the registered training vector (`torch.allclose` in Python; `|a - b| <= 1e-5 + 1e-5 * max(|a|, |b|)` in Rust). Both unseen names and known names with mod-edited stats resolve to `0` (`<UNK>`), setting `E_id = 0` so the category scores purely through `f(modifiers)`.

### Python → Rust JSON Export Contract

`ResidualCategory.export_document()` serializes a trained head into the exact schema loaded by `foch_eu4_decide::ResidualCategory::from_json` (`src/packages/foch-eu4-decide/src/residual.rs`):

| Field | Shape / Type | Description |
| :--- | :--- | :--- |
| `kind` | `"residual-category"` | Schema discriminator checked by `from_json` |
| `mechanics` | `list[str]` (len `5`) | Mechanic axis names in feature order |
| `names` | `list[str]` (len `N`) | Registered category names for indices `1..N` |
| `modifiers` | `[N, 5]` (`f32`) | Registered training-time mechanic rows |
| `f0_weight`, `f0_bias` | `[W, 5]`, `[W]` (`f32`) | First linear layer of `f` (`Linear(5, W)`) |
| `f2_weight`, `f2_bias` | `[W, W]`, `[W]` (`f32`) | Second linear layer of `f` (`Linear(W, W)`) |
| `embedding` | `[N + 1, W]` (`f32`) | `E_id` table; row `0` is `<UNK>` (`0.0`) |

---

## Reproducible Symmetric Joint-Training Ablation

`eu4_decide/_ablation.py` evaluates five arms trained on all `15 x 8 = 120` vanilla `(terrain, cb)` pairs and tested on four held-out decision distributions (`64` groups of `4` candidates each, top-2 reward margin `>= 0.35`, `EVAL_SEED = 20261120`):

- **`A`**: Least-squares one-hot ID lookup (`OneHotDecision`; unseen terrain $\to$ `plains`, unseen CB $\to$ `conquest`, modified numbers ignored).
- **`B`**: Pure mechanics (`f(modifiers)` with `E_id` fixed to `<UNK> = 0`).
- **`C0`, `C30`, `C50`**: `ResidualCategory` (`f(modifiers) + E_id`) with ID dropout $p \in \{0.0, 0.3, 0.5\}$.

### Symmetric Training Protocol

For each seed in `TRAIN_SEEDS = (0, 1, 2, 3, 4)`:
1. `B`, `C0`, `C30`, and `C50` start from the **exact same Xavier-uniform initialization** of `f` (`WIDTH = 16`, non-zero final layer, zero biases) and zero-initialized `E_id`.
2. `f` and `E_id` are optimized jointly in a **single AdamW parameter group** (`LR = 1e-3`, `WEIGHT_DECAY = 1e-2`).
3. Every neural arm runs for a fixed **`STEPS = 2000`** gradient steps with no warmup freeze of `f` and no early stopping.

### Verified 5-Seed Results (Mean across Seeds `0..4`)

| Distribution | Metric / Diagnostic | `A` (One-Hot) | `B` (`f` Only) | `C0` ($p=0.0$) | `C30` ($p=0.3$) | `C50` ($p=0.5$) |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: |
| `vanilla_stat_modified` | MSE $\downarrow$ | `4.0674` | `0.1783` | `0.5767` | **`0.1563`** | `0.1650` |
| `overhaul_unseen_cb` | MSE $\downarrow$ | `0.2667` | `0.0441` | `0.1565` | **`0.0344`** | `0.0376` |
| `overhaul_unseen_terrain` | Top-1 / Spearman $\uparrow$ | `0.2188` / `0.0250` | `1.0000` / `1.0000` | `1.0000` / `1.0000` | `1.0000` / `1.0000` | `1.0000` / `1.0000` |
| Parameter diagnostic | $\|E_{\text{id}}\|_2$ | — | `0.000` | `2.053` | `0.279` | `0.190` |

Key findings verified by `tests/test_ablation.py`:
- **ID shortcutting without dropout (`C0` vs. `C30`/`C50`)**: In `C0` ($p=0$), the embedding table absorbs the bulk of the training signal ($\|E_{\text{id}}\|_2 = 2.053$), leaving `f(modifiers)` under-trained. When modified stats (`vanilla_stat_modified`, where preference reversal reaches Spearman `-0.8357` on terrain and `-0.9048` on CB) or unseen CBs (`overhaul_unseen_cb`) trigger `<UNK>` fallback ($E_{\text{id}} = 0$), `C30` and `C50` cut `C0`'s MSE by more than $3.5\times$ (`0.5767` $\to$ `0.1563`/`0.1650` and `0.1565` $\to$ `0.0344`/`0.0376`) and shrink $\|E_{\text{id}}\|_2$ to `0.279` / `0.190`.
- **Unseen-terrain tie across neural arms**: On `overhaul_unseen_terrain`, each 4-candidate group holds the vanilla CB constant and varies only unseen terrains (`haunted_bog`, `dwarven_hold`, etc.), all of which resolve to `<UNK>`. Because `C0` still receives gradient updates on `f` from the shared Xavier init, `B`, `C0`, `C30`, and `C50` all tie at `Top-1 = 1.000` and `Spearman = 1.000` (with MSE `< 0.05` within seed noise), while one-hot `A` collapses to `Top-1 = 0.2188`.
- **Python → Rust numerical parity**: Seed `0` (`EXPORT_SEED = 0`) of `C50` exports to `src/packages/foch-eu4-decide/tests/fixtures/residual_ablation.json`. The Rust test `residual::tests::ablation_export_matches_python_on_every_distribution` verifies every exported terrain and CB case across all four distributions against the tolerance bound `< 1e-5`, observing a maximum absolute error of **`7.153e-7`**.

---

## Known Limits & Verification Boundary

- **Offline synthetic benchmark vs. live EU4**: The GRPO trainer (`_grpo.py`) and the residual-category ablation (`_ablation.py`) execute entirely offline against synthetic/simulated rewards (`env_server` and linear-plus-residual synthetic pools). They do not launch or benchmark live EU4 sessions.
- **Live memory boundary in `foch-eu4-state`**: Only the core layout fields provided by `foch_runtime::dataplane` (province array, base tax, base manpower, country registry, treasury, and packed tag) are live-verified against the pinned EU4 `1.37.5` executable SHA-256 digest (`supports_build` / `pinned_exe_sha256`).
- **Static-only candidate offsets**: Everything in `foch-eu4-state::candidates` — including country save-side army lists (`country::ARMY_LIST`), regiment strength (`regiment::STRENGTH`), combat-side regiment counts and unit vectors (`combat_side::*`), province controller/occupation/fort/production/terrain-template offsets, and warscore totals — is statically harvested from the `1.37.5` binary and has **not** been live-verified in a running EU4 session. All candidate reads are gated behind `VerifiedGame::new` and return `Option` (`None` on unknown or unverified state, never zero-filled); current regiment morale remains unpinned.
- **Read-only plugin**: `foch-plugin-read-decide` only reads gated state frames and logs GNN score suggestions; it never mutates EU4 memory or issues game commands.
