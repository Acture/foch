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
- crates.io: `foch` and `foch-cli`;
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

1. ☐ Release `tree-sitter-paradox` 0.3.0 from its own repository, at the commit
   the `src/packages/tree-sitter-paradox` gitlink records: the head of the
   `release/0.3.0` branch (`c0e946a` version bump, `0e8eab4` anchored include
   patterns). The branch is pushed to
   `Acture/tree-sitter-paradox`, which keeps the gitlink fetchable; it is not
   on that repository's `master` and is not published.
   - Configure the publishers its `package.yml` uses first: the crates.io
     trusted publisher (workflow `package.yml`, environment `release`), the
     PyPI trusted publisher for project `tree-sitter-paradox` (workflow
     `package.yml`, environment `pypi`) and its npm token. Its repository has
     the `crates`, `npm` and `pypi` environments; create `release`, optionally
     with you as required reviewer.
   - Push the branch, fast-forward its `master` to it and push the tag in one
     atomic push:
     `git -C src/packages/tree-sitter-paradox tag -a v0.3.0 -m v0.3.0`, then
     `git -C src/packages/tree-sitter-paradox push --atomic origin release/0.3.0:master v0.3.0`.
     Its `version-tag.yml` then finds the tag and skips. A tag that
     `version-tag.yml` pushes with `GITHUB_TOKEN` starts no workflow, so
     `package.yml` would never run; in that case publish the crate yourself
     with `cargo publish` from a clean checkout of `v0.3.0`.
   - Confirm crates.io lists it:
     `curl -fsS https://index.crates.io/tr/ee/tree-sitter-paradox | tail -n 1`.
   - Keep Foch's gitlink on that published commit. The preflight compares the
     crates.io `.crate` with `cargo package` of the checkout byte for byte.
2. ☐ Merge this source layout to `master` only together with the Homebrew
   tap's HEAD formula. `Acture/homebrew-ac` `packaging/homebrew/Formula/foch.rb`
   lists `public_submodules: %w[src/packages/tree-sitter-paradox vendor/cwtools-eu4-config]`
   for its `head` spec, and its download strategy runs
   `git submodule sync/update -- <paths>`, which fails the whole fetch on a
   path that no longer exists. In the same window as the merge, after step 1,
   change it to
   `%w[src/packages/tree-sitter-paradox src/packages/foch/vendor/cwtools-eu4-config]`
   (OSS-302 owns the tap).
3. ☐ Yank the superseded product: `cargo yank --version 0.1.0 foch`, with a
   crates.io API token that has the yank scope for `foch` (or after
   `cargo login`). Until then `cargo install foch` can install it, and the
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
     pin on `[workspace.dependencies] foch`;
   - `cargo update --workspace`, so `Cargo.lock` lists `foch`, `foch-cli` and
     `foch-desktop` at X.Y.Z;
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
12. ☐ Run `cargo acceptance` on that commit and confirm the fixed 14-case
    product acceptance completes. This Cargo alias is the only
    product-acceptance entrypoint; do not substitute a raw `cargo test`
    invocation. Record the result in [`project-status.md`](./project-status.md)
    with the date and exact commit. That record is a later, docs-only commit,
    so the tagged commit may differ from the accepted one only by it.
13. ☐ Run the read-only preflight from the release commit and expect every
    check to pass:
    `uv run --locked --project src/tools/foch-dev python -m foch_dev release preflight --tag vX.Y.Z`
    (with `GITHUB_TOKEN` exported). Then, from a clean checkout of that commit
    with both build submodules, run `cargo publish --dry-run --locked -p foch -p foch-cli`.
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
16. ☐ First release only, crates.io: Trusted Publishing cannot create
    `foch-cli`, so leave `CRATES_IO_PUBLISH` unset for this tag and publish by
    hand after the GitHub release job succeeds. Publishing earlier makes the
    preflight refuse the tag. From a clean checkout of the tag with both build
    submodules, create a crates.io API token limited to `foch` and `foch-cli`
    with the publish-new and publish-update scopes and a short expiry, run
    `cargo publish --locked -p foch -p foch-cli` with it in
    `CARGO_REGISTRY_TOKEN`, and revoke it. Then add a trusted publisher to both
    crates (owner `Acture`, repository `foch`, workflow `release.yml`,
    environment `crates-io`) and set `CRATES_IO_PUBLISH` for later releases.
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
    Foch" to match. Once the crates are published, also update "neither is
    published yet" under Repository layout, and the opening line of the
    project-status Distribution section. A channel without a passing run,
    including WinGet before its pull request merges, stays marked not
    available. This is a docs commit on `master`; do not retag.
22. ☐ Write the announcement from the verified release state and current public
    documentation.
