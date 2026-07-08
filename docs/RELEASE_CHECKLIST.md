# Alpha release checklist

Use this checklist when cutting the alpha release. Do not run the tag or publish steps from an autopilot agent unless the maintainer explicitly takes over the release workflow.

1. ☐ `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
2. ☐ `cargo test --workspace`
3. ☐ Update `Cargo.toml` workspace version to the alpha tag, for example `0.1.0-alpha.1`
4. ☐ Update `docs/project-status.md` "Last updated" date
5. ☐ Confirm the VS Code/LSP claim still matches [`lsp-0.1-preview.md`](./lsp-0.1-preview.md)
6. ☐ Tag: `git tag v0.1.0-alpha.1`
7. ☐ Push tags: `git push origin v0.1.0-alpha.1`
8. ☐ Build release artifacts: `cargo build --release --workspace`
9. ☐ Manually build the macOS Intel binary on an Intel Mac; this is not autopilot-safe because it requires the maintainer-side toolchain and hardware
10. ☐ Smoke-test the VS Code extension package: `bun run --cwd packages/vscode-foch test`
11. ☐ Build the VS Code extension package: `bun run --cwd packages/vscode-foch package:vsix`
12. ☐ Create the GitHub Release with binaries and the extension VSIX
13. ☐ Post [`ALPHA_ANNOUNCEMENT.md`](../ALPHA_ANNOUNCEMENT.md) to Discord and/or the forum
