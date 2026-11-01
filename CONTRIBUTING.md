# Contributing to Workbench

Issues and pull requests are welcome. Describe the concrete behavior you want to change, the steps to reproduce a bug and the expected result. Never attach credentials, private capability URLs, vault files or unredacted chat/provider logs.

## Local setup

Use Windows x64, Rust 1.96+, Node.js 24+, MSVC C++ build tools and WebView2. Clone the repository, run `npm ci`, then `npm run tauri -- dev`. Use a separate `WORKBENCH_DATA_DIR` for test profiles so development does not modify your normal account or chats.

Read [the architecture](docs/ARCHITECTURE.md) and [API contract](docs/API.md) before changing ownership between the frontend and backend. Rust owns credentials, model inference, policy and tool execution. The frontend renders the actual local API.

## Validation

Run the checks appropriate to the change:

```powershell
cargo fmt --all -- --check
cargo test -p workbench-core
cargo clippy --workspace --all-targets -- -D warnings
npm run check
npm run build
```

Use `npm run bundle` for native/packaging changes. A Docker integration check requires a running daemon and prepared image; its skip is explicit. Record live-account checks separately from deterministic provider fixtures. Documentation-only changes do not require rebuilding the app.

For new providers or MCP behavior, add fixtures that exercise real request/stream/tool boundaries. Preserve exact custom model IDs and destination-bound credentials. Do not substitute fabricated success responses in production, silently replay uncertain side effects or describe native subprocesses as sandboxed.

## Pull requests and licensing

Explain the problem, resulting behavior and relevant validation. Keep unrelated refactors separate. Original contributions are provided under the project's [MIT license](LICENSE); third-party material must retain its applicable license and attribution.
