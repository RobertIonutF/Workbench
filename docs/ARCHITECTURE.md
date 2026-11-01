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

## Version 0.2 runtime ownership

- `models.rs`: destination-bound model profiles, real capability/comparison requests, usage normalization, manual-price estimates and budgets.
- `context.rs`: scoped explicit attachments/mentions, lexical declarations, inherited memory, PDF extraction and provider-neutral image evidence.
- `tasks.rs`: pause/steering/queues, forks, plans, memory, policy-aware hooks, team orchestration and durable interval jobs. Scheduling serializes claims, persists before launch and disables interrupted work pending review. `App::begin_shutdown` gates new model runs and the scheduler before cancellation.
- `worktrees.rs`: libgit2 isolation/commits/integration, hash-checked hunk review and explicit PR publishing. Native Git publishing uses a host-owned bare repository and exact GitHub destination; project-controlled hooks/helpers/config are not inherited.
- `terminal.rs`: real portable-pty/ConPTY sessions, bounded output, attach/read/input/resize/stop and owned process cleanup.
- `studio.rs` plus `studio_plugins.rs`: actual developer trials, assertions/traces, source generation and pinned package catalogs. `studio_browser.rs` manages dev-server processes and isolated headless CDP browser checks. These execute native code explicitly and are not a security sandbox.
- `studio_backup.rs`: consistent SQLite snapshots, passphrase-encrypted portable archives, destination vault reencryption and pending restart restoration with recovery copies.
- `studio_updates.rs`: compiled Ed25519 trust root, verified manifests/digests and an exit-aware helper. Windows Authenticode signing uses a separately installed publisher certificate via the packaging script.
- `ControlPanel.tsx`, `TaskTools.tsx`, `Studio.tsx`, `CodeEditor.tsx` and `HunkReview.tsx`: API-backed controls; heavier editor/terminal/Studio code is loaded on demand.

The tray keeps the existing runtime alive only when the user enables background mode. Workbench is not a Windows service. Queues/periodic work resume from persisted state; interrupted external operations are not replayed automatically. A worktree inherits project memory and workflow configuration, while filesystem context resolves against its own checkout.

The feature guide documents deliberately bounded indexes, attachment limits, manual pricing, browser prerequisites and supported authentication routes. These are implementation boundaries, not placeholder features.
