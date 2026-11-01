# Releasing Workbench on Windows

Use Windows x64, Rust 1.96+, Node 24+, MSVC tools and WebView2. Keep `Cargo.toml`, `package.json`, `package-lock.json` and `src-tauri/tauri.conf.json` versions aligned, and update the changelog/catalog version.

## Checks and packages

```powershell
npm ci
cargo fmt --all -- --check
cargo test -p workbench-core --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm run check
npm audit --omit=dev --audit-level=high
npm run package
```

The package command builds deterministic starter catalog ZIPs first, updates their checked-in digests, builds the CLI and Tauri/NSIS desktop, and creates installer/portable/source ZIPs plus checksums under `output/release`. Source packaging uses a positive Git inventory and excludes runtime/test data. Review the inventory before publication. `node scripts/package.mjs --skip-build` repackages an already built release; `--hashes-only` refreshes checksums after signing an update manifest.

The GitHub Windows workflow runs the same checks and packaging and uploads artifacts. It has read-only repository permissions and no signing key. Publish only checked artifacts from the intended commit; do not push local backup branches or runtime profiles.

## Windows code signing

Set `WORKBENCH_CERTIFICATE_THUMBPRINT` to an installed trusted publisher certificate's thumbprint before `npm run package`. The build passes SHA-256 signing/timestamp settings to Tauri and verifies the resulting installer's Authenticode status. The certificate and its private key are not stored in the repository. Without one the build is unsigned; signed update metadata does not establish Windows publisher trust.

## Signed update feed

The app compiles `.github/workbench-update-key.pub`, a raw Ed25519 public key in base64. The matching PKCS8 PEM private key must remain outside the repo. Supply it with `WORKBENCH_UPDATE_PRIVATE_KEY_PEM`, or use this publisher machine's user-bound DPAPI file at `%LOCALAPPDATA%\WorkbenchPublisher\update-private.dpapi`. Do not log its value or publish the protected file.

```powershell
node scripts/sign-update.mjs
node scripts/package.mjs --hashes-only
```

Use `--stable` when publishing a stable release; preview is the default. The script verifies the private key matches the compiled public key and signs the exact installer SHA-256, version, platform, release URL, channel and notes. Upload `workbench-update.json` beside the installer. Replacing a release installer requires signing a new manifest and regenerating checksums. A key rotation requires a release that explicitly trusts the new key.

Create a `v<version>` GitHub release with the installer, portable/source ZIPs, all starter-plugin ZIPs, signed manifest and SHA256SUMS. Verify server-side asset digests against local hashes, and test the public update/catalog paths after publication. Keep preview releases marked prerelease.
