# Verification ledger

Verified locally on Windows x64, 1 November 2026. This ledger distinguishes actual local verification from account-dependent checks.

## Passed

- Rust core compilation and Windows Tauri desktop compilation.
- Provider SSE fragmentation, split UTF-8/CRLF, URL prefix handling, OAuth route request restrictions and incomplete-stream failure.
- Windows DPAPI credential round trip, encrypted-at-rest bytes, token replacement and deletion.
- Workspace traversal/secret path denial and undo conflict protection for newer user edits.
- Plugin manifest/skill discovery and invalid-skill isolation; ZIP traversal rejection.
- A real generated Python MCP process: initialize, tools/list, calculate tool invocation and correct result, shutdown, ZIP export and reinstallation.
- Native agent loop with deterministic HTTP provider: requested approval before a patch, successful approved write, denied write, read-only enforcement, malformed argument rejection and persistent chat history after restart.
- API requires session capability and rejects foreign origins. Custom model identifiers remain unchanged.
- Native command cancellation terminates a sleeping process promptly.
- Windows process lifecycle: five actual subprocess checks passed, including deadline, cancellation, bounded retained output, inherited-pipe descendants and abandoning an executing future. A Windows Job Object provides cleanup, not isolation; a small native spawn-to-attach interval remains.
- Plugin revocation during a pending launch prevents process execution. Git inspection uses libgit2; malicious repository fsmonitor/filter/textconv/external-helper fixtures never executed.
- A second runtime cannot invalidate live work. Interrupted calls reconcile without replay. The CLI reuses the locked desktop/server service and returns a real streamed response from a deterministic provider.
- Ordinary non-Git folders show actual checkpoint changes. Loaded LM Studio context metadata and embedding filtering passed fixture checks.
- MCP auth unit/fixture tests for metadata discovery, issuer/PKCE requirements, callback state/issuer/duplicate checks, destination binding, rotation and bearer storage/injection/removal.
- Frontend TypeScript check and production build.
- Full core suite: **33 passed, 0 failed**. Two entries are intentionally ignored: the internal subprocess helper and the Docker-daemon integration check. Workspace clippy passed with warnings denied; rustfmt passed. Production npm audit reported zero vulnerabilities.
- Generated Rust companion: optimized build and three generated-server tests passed. Real Rust and JavaScript stdio round trips calculated 1.25 + 2.75 = 4, and rejected unknown tools, methods and invalid/missing arguments. Python's actual tool round trip is also covered in the core integration suite.
- CLI development watcher installed an actual changed skill source at an inactive turn boundary.
- Actual browser workflows: connection save/manual model ID; project open; file browse/edit/save verified on disk; new task; combined plugin create/build/install/check/native handshake; skill source editing; ZIP creation; settings/diagnostics; 390px viewport without horizontal overflow.
- Visual inspection of the rendered plugin screen. The public screenshot is in `docs/images/`; local QA data and private session capability logs are excluded from source control.
- The existing-MCP creator wrapped an exact HTTP fixture URL, installed, validated, accepted a separate host token and discovered the fixture's real echo tool. Browser console errors: zero.
- An interactive ChatGPT OAuth sign-in completed successfully on Windows and persisted an `openai_oauth` connection. The profile reopened successfully. No live inference was performed with that account.

## External-state checks not established by these fixtures

- Paid model access, live OpenAI/Anthropic/Gemini inference, an actual LM Studio model, and provider-specific coding quality depend on configured accounts/models. No user's API credential was borrowed from another application.
- A live remote MCP OAuth account still needs user browser consent and eligible service configuration. Cryptographic/state/discovery tests are distinct from a successful live service authorization. ChatGPT sign-in was separately completed by the user as recorded above.
- Docker execution needs Docker and its explicitly prepared image; no native fallback is used. Its live cleanup test was skipped because a daemon was unavailable. Explicit cleanup can still fail if the daemon is unreachable or the host exits during cleanup. Native process execution is not an OS security sandbox.
- Public signing/automatic-update infrastructure and broad cross-platform releases are outside this Windows-first local release.

## Windows release

- Optimized native GUI and separate console CLI built successfully. NSIS installer generated successfully (about 11.24 MiB). Its extracted payload contains the UI, CLI and license notices.
- The extracted installer CLI matches the release CLI byte for byte. The GUI differs only in Tauri's expected three-byte bundle marker (`UNK` to `NSS`). PE subsystem checks confirm GUI/console separation.
- Launched the actual extracted installer GUI with an isolated data directory. Its WebView rendered Connections and reported **Local runtime connected**, with no loading/error screen. The bundled CLI installed a skill through this native instance and read it back from persistent state. A second GUI launch exited and focused the existing instance.
- The installer was extracted and its payload exercised; a clean-machine installation/upgrade/uninstall was not performed. This local build is unsigned.
- Release artifacts are in `output/release/`: installer, portable ZIP, source ZIP and `SHA256SUMS.txt`. The packaging inventory excludes runtime data/vaults, private QA logs, dependency caches and build intermediates. Archive audit and checksums accompany the artifacts.

The portable GUI embeds its UI. The accompanying `ui/` serves browser mode through the packaged CLI. Both use the same core. No credentials, model weights or account entitlement are bundled.
