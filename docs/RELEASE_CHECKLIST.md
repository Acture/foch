# Alpha release checklist

Use this checklist when cutting the alpha release. Do not run the tag or publish steps from an autopilot agent unless the maintainer explicitly takes over the release workflow.

1. ☐ `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
2. ☐ `cargo test --workspace`
3. ☐ Update `Cargo.toml` workspace version to the alpha tag, for example `0.1.0-alpha.1`
4. ☐ Update `docs/project-status.md` "Last updated" date
5. ☐ Tag: `git tag v0.1.0-alpha.1`
6. ☐ Push tags: `git push origin v0.1.0-alpha.1`
7. ☐ Build release artifacts: `cargo build --release --workspace`
8. ☐ Manually build the macOS Intel binary on an Intel Mac; this is not autopilot-safe because it requires the maintainer-side toolchain and hardware
9. ☐ Build the VS Code extension package: `cd packages/vscode-foch && bun run package`
10. ☐ Create the GitHub Release with binaries and the extension VSIX
11. ☐ Post [`ALPHA_ANNOUNCEMENT.md`](../ALPHA_ANNOUNCEMENT.md) to Discord and/or the forum
