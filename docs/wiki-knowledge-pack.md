# Frozen modding-wiki knowledge packs

`foch-mq knowledge` builds and queries a revision-addressed documentation
snapshot. The first profile is `eu4-modding`, selected from mainspace links in
the rendered `Template:Modding navbox`.

The pack is advisory context. It is not a merge oracle and cannot by itself
justify an accepted `foch_better` adjudication. Installed game data, versioned
foch semantics, runtime evidence, and bound AST invariants take precedence.

## Acquisition

Build the network-enabled CLI:

```fish
cargo build -p foch-merge-quality --bin foch-mq --features wiki
```

Run the full snapshot manually. Acquisition is intentionally serial and
observes a 15-second request interval, so the current 71-page profile is a
long-running command:

```fish
target/debug/foch-mq \
	--dataset-root crates/foch-merge-quality/dataset \
	knowledge snapshot \
	--profile eu4-modding \
	--game-version auto
```

The direct MediaWiki API is attempted first. If the site returns its current
client challenge instead of JSON, the process records the failure and uses the
configured `r.jina.ai` MediaWiki-API transport for the remaining requests. Use
`--no-jina-fallback` to require direct API access.

The default archive is under
`crates/foch-merge-quality/dataset/.work/knowledge/` and is Git-ignored. Do not
commit Wiki text or rendered HTML to the main repository.

## Archive contract

The deterministic `.tar.zst` archive has a 30 MiB hard limit and contains:

- semver schema `1.0.0` and a BLAKE3 pack identity;
- the exact game-version binding and navbox revision;
- one current revision per selected page;
- raw wikitext and official rendered HTML;
- section, page, revision, parent, timestamp, contributor, and URL metadata;
- per-content hashes plus CC BY-SA 3.0 notice and attribution.

Images, full revision histories, and embeddings are excluded. Sorted entries
and fixed tar metadata make repeated packaging of identical page records
byte-identical.

## Verification and search

Verification is offline:

```fish
target/debug/foch-mq knowledge verify \
	--archive crates/foch-merge-quality/dataset/.work/knowledge/eu4-modding-v1.37.5.0.tar.zst
```

Search builds a derived content-addressed chunk cache under
`dataset/.work/knowledge/<pack-id>/<chunker-version>/`. It uses in-memory BM25
and preserves Clausewitz identifiers such as `country_event`,
`event_target:owner`, and `common/scripted_effects/example.txt`.

```fish
target/debug/foch-mq \
	--dataset-root crates/foch-merge-quality/dataset \
	knowledge search \
	--archive crates/foch-merge-quality/dataset/.work/knowledge/eu4-modding-v1.37.5.0.tar.zst \
	--limit 5 \
	--max-chars 6000 \
	"replace_path definition module"
```

Every hit includes its page and exact revision identity. Search bounds are hard
caps, not prompt suggestions.
