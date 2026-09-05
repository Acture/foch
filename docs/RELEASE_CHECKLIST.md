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
8. ☐ Tag: `git tag v0.0.1`
9. ☐ Push tags: `git push origin v0.0.1`
10. ☐ Build release artifacts: `cargo build --release --workspace`
11. ☐ Manually build the macOS Intel binary on an Intel Mac; this requires the
    maintainer-side toolchain and hardware.
12. ☐ Smoke-test the VS Code extension package:
    `bun run --cwd packages/vscode-foch test`
13. ☐ Build the VS Code extension package:
    `bun run --cwd packages/vscode-foch package:vsix`
14. ☐ Create the GitHub Release with binaries and the extension VSIX.
15. ☐ Write a fresh announcement from the verified release state.
    [`ALPHA_ANNOUNCEMENT.md`](../ALPHA_ANNOUNCEMENT.md) is archived historical
    material and must not be posted as-is.
