# Alpha release checklist

Use this checklist when cutting the alpha release. Do not run the long
real-Workshop acceptance gate, tag, or publish steps from an autopilot agent;
the maintainer must take over those parts of the release workflow.

1. ☐ `cargo fmt --all --check`
2. ☐ `cargo clippy --workspace --all-targets --all-features -- -D warnings`
3. ☐ `cargo test --workspace`
4. ☐ Have the maintainer run `cargo acceptance` and confirm
   the fixed 14-case product acceptance completes successfully. This Cargo alias is
   the only product-acceptance entrypoint; do not substitute a raw `cargo test`
   invocation.
5. ☐ Update the `Last verified` line in
   [`project-status.md`](./project-status.md) with the verification date and
   exact commit, and record the acceptance result there.
6. ☐ Confirm the `Cargo.toml` workspace version is `0.0.1`.
7. ☐ Confirm the VS Code/LSP claim still matches
   [`lsp-0.1-preview.md`](./lsp-0.1-preview.md).
8. ☐ Confirm `gh secret list --repo Acture/foch` lists `HOMEBREW_TAP_TOKEN`: a
   fine-grained token for `Acture/homebrew-ac` only, with Contents read and
   write. The release workflow refuses to publish while the `HOMEBREW_TAP_REPO`
   target lacks the secret; it does not test the token's permissions.
9. ☐ Tag: `git tag v0.0.1`
10. ☐ Push tags: `git push origin v0.0.1`
11. ☐ Build release artifacts: `cargo build --release --workspace`
12. ☐ Manually build the macOS Intel binary on an Intel Mac; this requires the
    maintainer-side toolchain and hardware.
13. ☐ Smoke-test the VS Code extension package:
    `bun run --cwd src/apps/vscode-foch test`
14. ☐ Build the VS Code extension package:
    `bun run --cwd src/apps/vscode-foch package:vsix`
15. ☐ Create the GitHub Release with binaries and the extension VSIX.
16. ☐ Confirm the release workflow's Homebrew tap job committed
    `packaging/homebrew/Formula/foch.rb` for this version, and that the tap's
    Brew CI installed and tested it on macOS and Linux. To retry the sync for an
    existing release: `gh workflow run sync-homebrew-tap.yml -f tag=v0.0.1`.
    The first sync replaces the HEAD-only formula, so the tap's README and
    status must then list the stable `brew install acture/ac/foch`.
17. ☐ Write an announcement from the verified release state and current public
    documentation.
