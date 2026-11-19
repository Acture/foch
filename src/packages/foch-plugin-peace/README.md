# Bilateral peace window

This unpublished Windows x64 plugin is an incomplete experiment for EU4
1.37.5. It keeps the native demand and concession drafts and displays both in
the peace description, including a `NET DEAL` line that subtracts the given
side from the demanded side. The two tabs select which side to edit.

**Mixed offers are sent as one merged, net-priced offer.** Submitting with
terms on both sides clones the inactive side's selected terms into the active
offer, re-applies the give side's clauses through the native signed cost
accumulator so the merged offer's cached total becomes the net deal value
(demands minus concessions) that native validation and AI evaluation read,
lets the original submission copy that offer into the peace action, and then
restores the editor's own selection. The merged clones stay owned by the
editor's offer until it is destroyed; nothing is freed twice.

While both drafts have terms, the summary hook also writes the net value
into the active draft's cached total, so the native cost display and the
`NET DEAL` line in the agreement text both show the merged deal value; a
single-direction selection keeps its own display.

Every executed peace offer is logged with its clause count, parties,
direction and cached score. A merged offer whose inactive-side clauses
vanish between submission and execution is visible in that log line
instead of as a silent one-direction treaty.

The pricing chain above is verified by disassembly of the pinned image only.
Whether the receiving AI re-runs the all-positive native rebuild on its copy
of the offer — discarding the net total — and how a mixed treaty settles in
every clause family are **not verified in game**. Test only in a disposable
EU4 campaign profile. Startup alone does not verify any of this.

Build with:

```powershell
cargo build -p foch-plugin-peace -p foch-eu4-host
cargo test -p foch-plugin-peace --lib
```

For a local package, place `foch-plugin.toml` and the built
`foch_plugin_peace.dll` in one directory, then import it with
`foch plugin import <directory>`. Use a disposable EU4 profile and an isolated
Foch runtime for development. It is not a Workshop or release package.
The exact executable and patched instructions are checked before activation;
other builds are refused.
