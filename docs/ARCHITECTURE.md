# Workbench architecture

Workbench has one Rust application service, a Tauri desktop shell, a React interface and a CLI. The UI does not hold provider keys or execute agent tools. The CLI can reuse the running desktop service through an encrypted, local capability record.

`crates/workbench-core/src/app.rs` owns project, connection, chat and settings use cases. `store.rs` keeps SQLite entities, messages, ordered events and file preimages. A data-directory lock prevents a second runtime from applying recovery to live work. Interrupted tool calls receive an explicit uncertain-outcome record, without automatic replay.

`providers.rs` translates canonical messages and tool definitions into OpenAI Chat Completions, OpenAI Responses, Anthropic Messages or Gemini requests. It parses streamed fragments and requires a terminal success. Compatible endpoints use exact configured model IDs. OAuth route restrictions and provider-native continuation fields stay in their adapters. Models without tool support can still chat.

`runtime.rs` runs one active turn per chat. It budgets context, selects skills, discovers tools, validates arguments, requests approvals and persists tool results before continuing inference. Built-in file operations resolve paths inside the selected project. Native commands have bounded output, deadlines and cancellation; Windows Job Objects close process descendants. Docker mode uses a prepared image and explicit container cleanup. Neither native commands nor native MCP servers are security sandboxes.

`plugins.rs` validates manifests, skills and MCP configuration, installs copied versions, creates templates, builds Rust companions and exports ZIPs. A plugin contains instructions and tool connections; it does not choose the model or grant itself authority. The runtime rechecks enablement and package identity after approvals. Large MCP catalogs use deferred local search.

`mcp.rs` owns stdio/Streamable HTTP sessions and MCP tool discovery/invocation. `mcp_auth.rs` separately owns remote-server bearer/OAuth credentials. `auth.rs` owns model-provider sign-in and the Windows DPAPI vault. Authorization is bound to a destination and is never copied into public connection/plugin JSON.

`api.rs` provides the authenticated loopback API used by all interfaces. Tauri exposes only the bootstrap command to its local WebView. Browser mode receives a one-time URL fragment and removes it from the address bar. `src/` renders persisted state and live events through this API; production has no simulated model responses.

To add a provider, implement real catalog/request/stream behavior in `providers.rs`, add a connection preset and meaningful fixtures, and then perform an account-dependent live check. A different authentication protocol needs its own adapter; an arbitrary URL cannot supply it. A new domain workflow usually belongs in a skill/MCP plugin and requires no app rebuild.

Run `npm run bundle` for the Windows installer. `scripts/package.ps1` also produces portable/source ZIPs and SHA-256 hashes. See `VERIFICATION.md` for checks actually completed on this release.
