# Parser and decoder fuzzing

This independent Cargo workspace contains `cargo-fuzz` property harnesses for
the Clausewitz parser and Paradox text decoder in the adjacent Foch library.

## Install

```fish
cargo install cargo-fuzz
```

## Run

Run from the library directory:

```fish
cd src/packages/foch
# Build all targets
cargo +nightly fuzz build

# Run a target locally for 10 minutes
cargo +nightly fuzz run property_clausewitz_parser -- -max_total_time=600

# Triage a finding
cargo +nightly fuzz fmt property_clausewitz_parser fuzz/artifacts/property_clausewitz_parser/<artifact>
```

Replace `property_clausewitz_parser` with any target listed in `fuzz/Cargo.toml`.

## Corpora and artifacts

Seed corpora live in `fuzz/corpus/<target>/`. Keep seeds small and hand-crafted; do not add large real-world mods.

Crash artifacts are written to `fuzz/artifacts/<target>/`. To triage, format the artifact with `cargo +nightly fuzz fmt`, reproduce it with `cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<artifact>`, then file a follow-up with the target name, artifact, reproducing command, and panic details.
