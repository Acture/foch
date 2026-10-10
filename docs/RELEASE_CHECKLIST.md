# Release runbook

Agents prepare, build and verify. Only the maintainer runs the long
real-Workshop acceptance gate, tags, publishes, yanks, registers publishers,
changes other repositories or submits to WinGet. Pushing a releasable tag
publishes the GitHub release and, once the `HOMEBREW_TAP_TOKEN` secret exists,
updates the Homebrew tap. crates.io and PyPI publish only when their repository
variable is `true` and you approve their environment. WinGet is submitted by
hand.

One tag and its source release to every channel:

- the GitHub release: `foch-<version>-<target>.tar.gz` (`.zip` for
  `win32-x64`) for `linux-x64`, `darwin-arm64` and `win32-x64`, the PyPI wheels,
  the VSIX packages, `foch-<version>-source.tar.gz`,
  `foch-<version>-winget-manifests.zip` and `SHA256SUMS.txt`;
- crates.io: `foch-cli` and every workspace crate it builds from, `foch`,
  `foch-annotation`, `foch-lsp`, `foch-runner` and `foch-test`
  (`python -m foch_dev check` derives and enforces the set);
- PyPI: the `foch` wheels;
- WinGet: `Acture.Foch`, by pull request to `microsoft/winget-pkgs`;
- Homebrew: the tap that `sync-homebrew-tap.yml` updates from the source archive.

The wheels, the archives and WinGet share one binary per target. The VSIX
builds its own, and crates.io and Homebrew compile from source. Releasable tags
are `vX.Y.Z` and `vX.Y.Z-(alpha|beta|rc).N`, naming the `[workspace.package]`
version. Every channel must install a `foch` whose `--version` prints
`foch-cli <version>` and the `cwt-schema` id embedded at the tag. Releases
carry no EU4 base data; users build it from their own game installation.

## One-time setup

1. ☐ Release `tree-sitter-paradox` 0.3.0 from its own repository through
   [Acture/tree-sitter-paradox#12](https://github.com/Acture/tree-sitter-paradox/pull/12)
   (`release/0.3.0`: the CWT grammar, the 0.3.0 version bump, anchored crate
   include patterns, dispatch of its publishing workflows, and the review fix
   that ends inline `--[[ ]]` comments before following code). Foch's gitlink
   records the tagged `v0.3.0` commit `9a21dfc` (the squashed merge, same tree
   as `d7999af`); with it the embedded CWT rule pack is byte-identical to the
   one built from the previous grammar.
   - Configure the publishers its `package.yml` uses first: the crates.io
     trusted publisher for `tree-sitter-paradox` (workflow `package.yml`,
     environment `release`), the PyPI trusted publisher for project
     `tree-sitter-paradox` (workflow `package.yml`, environment `pypi`) and its
     `NPM_TOKEN` secret.
   - Merge the pull request. `version-tag.yml` then tags `v0.3.0` and
     dispatches `package.yml` and `release.yml` on that tag, because a tag it
     pushes with `GITHUB_TOKEN` starts no `push` workflow.
   - Confirm crates.io lists it:
     `curl -fsS https://index.crates.io/tr/ee/tree-sitter-paradox | tail -n 1`.
   - Move Foch's gitlink to the released commit
     (`git -C src/packages/tree-sitter-paradox fetch --tags origin` and
     `git -C src/packages/tree-sitter-paradox checkout v0.3.0`) and commit it.
     The preflight compares the crates.io `.crate` with `cargo package` of the
     checkout byte for byte.
2. ☐ Merge this source layout to `master` only together with the Homebrew
   tap's HEAD formula. `Acture/homebrew-ac` `packaging/homebrew/Formula/foch.rb`
   lists `public_submodules: %w[src/packages/tree-sitter-paradox vendor/cwtools-eu4-config]`
   for its `head` spec, and its download strategy runs
   `git submodule sync/update -- <paths>`, which fails the whole fetch on a
   path that no longer exists. In the same window as the merge, after step 1,
   change it to
   `%w[src/packages/tree-sitter-paradox src/packages/foch/vendor/cwtools-eu4-config]`
   (OSS-302 owns the tap).
3. ☑ Yank the superseded product: `cargo yank --version 0.1.0 foch` (done on
   2026-10-08). While it is unyanked, `cargo install foch` installs it and the
   preflight refuses to publish.
4. ☐ In the GitHub repository settings, create the environments `crates-io` and
   `pypi`, each with you as required reviewer: that approval is the
   authorization for every registry upload. Set the repository variables
   `CRATES_IO_PUBLISH` and `PYPI_PUBLISH` to `true` only when a release should
   publish there. The Homebrew sync needs `HOMEBREW_TAP_REPO` and the
   `HOMEBREW_TAP_TOKEN` secret, a fine-grained token for `Acture/homebrew-ac`
   only with Contents read and write; confirm it with
   `gh secret list --repo Acture/foch`. The release workflow refuses to publish
   while the `HOMEBREW_TAP_REPO` target lacks the secret, but does not test the
   token's permissions. The sync has no environment and also runs for
   pre-release tags.
5. ☐ Register the PyPI pending publisher for project `foch`: owner `Acture`,
   repository `foch`, workflow `release.yml`, environment `pypi`. It does not
   reserve the name; the first upload creates the project.

## Every release

6. ☐ Set the version before any gate, so every gate runs on the commit you tag:
   - in `Cargo.toml`, `[workspace.package] version` and the `version = "=X.Y.Z"`
     pins on the five published path dependencies in `[workspace.dependencies]`
     (`foch_dev check` refuses a stale one);
   - `cargo update --workspace`, so `Cargo.lock` lists every workspace crate
     except `tree-sitter-paradox` at X.Y.Z;
   - `THIRD-PARTY-LICENSES.txt`, which names the crate versions: regenerate it
     with cargo-about 0.9.2 as `src/apps/foch-cli/about.toml` describes;
   - both version mentions in the README (the banner and "The Rust product is
     versioned at") and the version in [`project-status.md`](./project-status.md).

   `foch-desktop` is not released by this runbook; its `tauri.conf.json` and
   `package.json` versions are not release identities. Commit and push.
7. ☐ `cargo fmt --all --check`
8. ☐ `cargo clippy --workspace --all-targets --all-features -- -D warnings`
9. ☐ `cargo test --workspace`
10. ☐ `uv run --locked --project src/tools/foch-dev python -m foch_dev check`
    and the foch-dev package checks in its
    [README](../src/tools/foch-dev/README.md).
11. ☐ `bun run --cwd src/apps/vscode-foch test`, and confirm the VS Code/LSP
    claim still matches [`lsp-0.1-preview.md`](./lsp-0.1-preview.md).
12. ☐ Record the product-acceptance state of the release in
    [`project-status.md`](./project-status.md). Acceptance does not gate an
    alpha release (decided 2026-10-09): a release establishes installation,
    and its notes link that page for merge quality. Where `cargo acceptance`
    can run, run it on that commit and record the date, exact commit and
    result; that record is a later, docs-only commit. This Cargo alias is the
    only product-acceptance entrypoint; do not substitute a raw `cargo test`
    invocation, and never describe a release as merge-ready without a complete
    accepted cohort.
13. ☐ Run the read-only preflight from the release commit and expect every
    check to pass:
    `uv run --locked --project src/tools/foch-dev python -m foch_dev release preflight --tag vX.Y.Z`
    (with `GITHUB_TOKEN` exported). Then, from a clean checkout of that commit
    with both build submodules, run
    `cargo publish --dry-run --locked -p foch -p foch-annotation -p foch-test -p foch-lsp -p foch-runner -p foch-cli`.
    It needs no token and builds the crates against the crates.io grammar, as
    the real publish does; `crate-smoke` packages the grammar locally instead.
14. ☐ Tag and push: `git tag -a vX.Y.Z -m "Foch X.Y.Z"`, then
    `git push origin vX.Y.Z`.
15. ☐ Watch `release.yml`: the preflight, then `dist.yml` (wheels and archives
    with their uv, crate and WinGet install smoke, each required to report the
    tag's version and CWT schema id, and the third-party license check), the
    VSIX and source archive, the GitHub release, the publish jobs (approve
    their environments), the WinGet release verification and the Homebrew
    sync. If a job fails after the release has assets, use "Re-run failed
    jobs": the preflight refuses a published release that already has assets.
    A draft left by an interrupted upload passes the preflight; the release job
    then keeps its identical assets, adds the missing ones, fails if one
    differs, and publishes it. Base-data assets added later with
    `scripts/upload_release_data_assets.sh` are not part of this comparison.
16. ☐ crates.io, whenever the preflight notes that crates.io has no crate of
    a published one yet: Trusted Publishing cannot create a crate, so that
    release publishes its crates by hand. On the first release five of the six
    do not exist yet: `foch-cli`, `foch-annotation`, `foch-lsp`, `foch-runner`
    and `foch-test` (`foch` exists through the yanked 0.1.0). The same holds
    for a later release after `foch-cli` gains a workspace dependency; with
    `CRATES_IO_PUBLISH` set, its crate selection in `release.yml` fails before
    anything is uploaded. Leave the variable unset for such a tag, or let that
    job fail, and publish by hand after the GitHub release job succeeds.
    Publishing earlier makes the preflight refuse the tag.
    - crates.io lets an account create a burst of 5 new crates, then 1 more
      every 10 minutes. The first release's five fit in one burst only if the
      account has not created other crates shortly before; otherwise expect
      the limit to stop the publish part-way.
    - From a clean checkout of the tag with both build submodules, create a
      crates.io API token limited to the published crates (on the first
      release `foch`, `foch-annotation`, `foch-cli`, `foch-lsp`, `foch-runner`
      and `foch-test`), with the publish-new and publish-update scopes and a
      short expiry. With it in `CARGO_REGISTRY_TOKEN`, run
      `cargo publish --locked` with `-p` for each crate that
      `uv run --locked --project src/tools/foch-dev python -m foch_dev release unpublished --allow-new`
      prints; on the first release that is
      `cargo publish --locked -p foch -p foch-annotation -p foch-test -p foch-lsp -p foch-runner -p foch-cli`.
      cargo publishes them in dependency order and waits for each to reach
      the index.
    - Publishing several crates is not atomic. If the rate limit or anything
      else stops it, wait as crates.io says and re-run the command with only
      the crates still missing, which `release unpublished --allow-new`
      prints. Then revoke the token.
    - Add a trusted publisher to each new crate (owner `Acture`, repository
      `foch`, workflow `release.yml`, environment `crates-io`) and set
      `CRATES_IO_PUBLISH` for later releases, whose `release.yml` publishes
      exactly the crates `release unpublished` lists. If that job failed for
      this tag, "Re-run failed jobs" now finds nothing left to publish.
17. ☐ WinGet: after `winget-release-verify` installed from them, submit the
    three files of the release asset `foch-<version>-winget-manifests.zip`
    (`manifests/a/Acture/Foch/<version>/`) unchanged as one pull request to
    `microsoft/winget-pkgs`, titled
    `New package: Acture.Foch version <version>` for the first submission.
    Sign the Microsoft CLA when its bot asks. State in the body that `foch.exe`
    without arguments prints usage and exits 2, so validation should run
    `foch.exe --version`. Review can take weeks; later versions can be
    automated only after the package exists.
18. ☐ Homebrew: confirm the release workflow's Homebrew tap job committed
    `packaging/homebrew/Formula/foch.rb` for this version, and that the tap's
    Brew CI installed and tested it on macOS and Linux. To retry the sync for an
    existing release: `gh workflow run sync-homebrew-tap.yml -f tag=vX.Y.Z`.
    The first sync replaces the HEAD-only formula, so the tap's README and
    status must then list the stable `brew install acture/ac/foch`.
19. ☐ After each channel has published, run the post-publication check on clean
    runners for the live channels, for example
    `gh workflow run verify-install.yml -f tag=vX.Y.Z -f channels=cargo,uv`.
    WinGet is live only after its pull request merges.
20. ☐ Record each channel's verify-install run and result in the Distribution
    section of [`project-status.md`](./project-status.md). Installation
    results and merge-quality acceptance are separate facts; record them
    separately.
21. ☐ For each channel whose verify-install run passed, update the README
    Install section: mark that channel available, and rewrite the "Not
    available yet" banner and "Building from source is the current way to run
    Foch" to match. Once the crates are published, also update "none is
    published yet" under Repository layout, and the opening line of the
    project-status Distribution section. A channel without a passing run,
    including WinGet before its pull request merges, stays marked not
    available. This is a docs commit on `master`; do not retag.
22. ☐ Write the announcement from the verified release state and current public
    documentation.
