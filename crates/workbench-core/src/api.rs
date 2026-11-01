use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

pub async fn start(app: Arc<App>, port: u16, ui: Option<PathBuf>) -> Result<(SocketAddr, String)> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let address = listener.local_addr()?;
    let token = app.token.clone();
    crate::features::start_services(app.clone());
    crate::auth::set_secret(
        &app,
        "runtime",
        &json!({"baseUrl":format!("http://{address}"),"token":token}).to_string(),
    )?;
    let mut router = Router::new()
        .route("/api/{*path}", any(handle))
        .route(
            "/health",
            axum::routing::get(|| async {
                axum::Json(json!({"name":"Workbench","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .with_state(app);
    let installed_ui = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("ui")))
        .filter(|path| path.join("index.html").is_file());
    let default_ui = installed_ui
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dist"));
    let ui = ui.unwrap_or(default_ui);
    if ui.join("index.html").exists() {
        router =
            router
                .fallback_service(tower_http::services::ServeDir::new(&ui).not_found_service(
                    tower_http::services::ServeFile::new(ui.join("index.html")),
                ));
    }
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("Local server stopped: {e}");
        }
    });
    Ok((address, token))
}
async fn handle(State(app): State<Arc<App>>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().trim_start_matches("/api/").to_owned();
    let query: HashMap<String, String> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let allowed_origin = origin.as_deref().is_none_or(|o| {
        o == "tauri://localhost"
            || o == "http://tauri.localhost"
            || o == "https://tauri.localhost"
            || url::Url::parse(o).is_ok_and(|url| {
                url.scheme() == "http"
                    && url.host_str() == Some("127.0.0.1")
                    && url.port().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
            || (cfg!(debug_assertions) && o == "http://localhost:1420")
    });
    if !allowed_origin {
        return error(StatusCode::FORBIDDEN, "Origin is not allowed");
    }
    if method == Method::OPTIONS {
        return cors(StatusCode::NO_CONTENT.into_response(), origin.as_deref());
    }
    if request
        .headers()
        .get("X-Workbench-Token")
        .and_then(|h| h.to_str().ok())
        != Some(app.token.as_str())
    {
        return cors(
            error(
                StatusCode::UNAUTHORIZED,
                "Local session token is missing or invalid",
            ),
            origin.as_deref(),
        );
    }
    let result = async {
        let bytes = to_bytes(request.into_body(), 24 * 1024 * 1024).await?;
        let value = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes).context("Invalid request JSON")?
        };
        dispatch(app.clone(), &method, &path, &query, value).await
    }
    .await;
    let response = match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(e) => {
            let safe =
                crate::studio::redact(&json!(e.to_string()), &crate::studio::known_secrets(&app));
            error(
                StatusCode::BAD_REQUEST,
                safe.as_str().unwrap_or("Request failed"),
            )
        }
    };
    cors(response, origin.as_deref())
}
fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({"error":message}))).into_response()
}
fn cors(mut response: Response, origin: Option<&str>) -> Response {
    if let Some(origin) = origin
        && let Ok(value) = origin.parse()
    {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static("Content-Type, X-Workbench-Token"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
fn query<'a>(q: &'a HashMap<String, String>, key: &str) -> &'a str {
    q.get(key).map(String::as_str).unwrap_or("")
}
async fn dispatch(
    app: Arc<App>,
    method: &Method,
    path: &str,
    q: &HashMap<String, String>,
    mut value: Value,
) -> Result<Value> {
    if *method != Method::GET && app.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
        bail!("Workbench is shutting down");
    }
    if path.starts_with("studio/") {
        return crate::studio::dispatch(app, method, path, q, value)
            .await
            .context("Unknown Studio endpoint")?;
    }
    if path.starts_with("features/") {
        return crate::features::dispatch(app, method, path, q, value).await;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let get = *method == Method::GET;
    let post = *method == Method::POST;
    let delete = *method == Method::DELETE;
    match parts.as_slice() {
        ["state"] if get => app.state(),
        ["connections"] if post => app.connection(value),
        ["connections", connection_id] if delete => {
            for c in app.store.all("chat")? {
                if c["connectionId"] == *connection_id
                    && app
                        .store
                        .all("run")?
                        .iter()
                        .any(|r| r["chatId"] == c["id"] && crate::tasks::active(r))
                {
                    bail!("Cancel the connection's active runs first")
                }
            }
            crate::auth::delete_secret(&app, connection_id)?;
            crate::auth::delete_secret(&app, &format!("{connection_id}-oauth"))?;
            app.store.delete("connection", connection_id)?;
            Ok(json!({"ok":true}))
        }
        ["connections", connection_id, "models"] if post => {
            crate::providers::models(&app, &app.store.get("connection", connection_id)?).await
        }
        ["connections", connection_id, "probe"] if post => {
            let mut connection = app.store.get("connection", connection_id)?;
            let model = if text(&value, "modelId").is_empty() {
                text(&connection, "modelId").to_owned()
            } else {
                text(&value, "modelId").to_owned()
            };
            if model.is_empty() {
                bail!("Choose or enter a model ID before testing")
            };
            let result = crate::providers::probe(&app, &connection, &model).await?;
            connection["tools"] = result["tools"].clone();
            connection["probe"] = json!({"verifiedAt":now(),"modelId":model,"text":result["text"],"streaming":result["streaming"],"tools":result["tools"]});
            app.store.put("connection", &connection)?;
            Ok(result)
        }
        ["oauth", "openai", "start"] if post => crate::auth::start(app).await,
        ["oauth", login_id] if get => Ok(app
            .oauth
            .lock()
            .get(*login_id)
            .cloned()
            .unwrap_or(json!({"status":"unknown"}))),
        ["projects"] if post => app.project(value),
        ["projects", project_id] if delete => {
            if app
                .store
                .all("chat")?
                .iter()
                .any(|c| c["projectId"] == *project_id)
            {
                bail!("Delete or move chats using this project first")
            };
            app.store.delete("project", project_id)?;
            Ok(json!({"ok":true}))
        }
        ["chats"] if post => app.chat(value),
        ["chats", chat_id] if *method == Method::PATCH => {
            if app
                .store
                .all("run")?
                .iter()
                .any(|r| r["chatId"] == *chat_id && crate::tasks::active(r))
            {
                bail!("Wait for or cancel the active turn before changing its configuration")
            };
            let mut chat = app.store.get("chat", chat_id)?;
            for (k, v) in value.as_object().context("Expected an object")? {
                if !["id", "createdAt"].contains(&k.as_str()) {
                    chat[k] = v.clone();
                }
            }
            app.chat(chat)
        }
        ["chats", chat_id] if delete => {
            if app
                .store
                .all("run")?
                .iter()
                .any(|r| r["chatId"] == *chat_id && crate::tasks::active(r))
            {
                bail!("Cancel this chat's active run before deleting it")
            };
            app.store.delete_chat(chat_id)?;
            Ok(json!({"ok":true}))
        }
        ["chats", chat_id, "messages"] if get => Ok(json!(app.store.messages(chat_id)?)),
        ["chats", chat_id, "turns"] if post => {
            crate::runtime::start(app, chat_id, text(&value, "text"))
        }
        ["runs", run_id, "cancel"] if post => {
            if let Some(token) = app.runs.lock().get(*run_id) {
                token.cancel();
            }
            Ok(json!({"ok":true}))
        }
        ["events"] if get => app.store.events(
            query(q, "after").parse().unwrap_or(0),
            q.get("chatId").map(String::as_str),
        ),
        ["approvals", approval_id] if post => {
            let accepted = text(&value, "decision") == "allow_once";
            let sender = app
                .approvals
                .lock()
                .remove(*approval_id)
                .context("Approval is no longer pending")?;
            let _ = sender.send(accepted);
            Ok(json!({"ok":true}))
        }
        ["workspace", "files"] if get => {
            let project = app.store.get("project", query(q, "projectId"))?;
            crate::workspace::list(Path::new(text(&project, "path")), query(q, "path"))
        }
        ["workspace", "file"] if get => {
            let project = app.store.get("project", query(q, "projectId"))?;
            Ok(
                json!({"path":query(q,"path"),"content":crate::workspace::read(Path::new(text(&project,"path")),query(q,"path"))?}),
            )
        }
        ["workspace", "file"] if *method == Method::PUT => {
            let project = app.store.get("project", query(q, "projectId"))?;
            let chat_id = text(&value, "chatId");
            if !chat_id.is_empty() && app.store.get("chat", chat_id)?["projectId"] != project["id"]
            {
                bail!("Task and file must belong to the same project");
            }
            crate::workspace::write(
                &app,
                chat_id,
                Path::new(text(&project, "path")),
                query(q, "path"),
                text(&value, "content"),
                value["expectedContent"].as_str(),
            )?;
            Ok(json!({"ok":true}))
        }
        ["workspace", "diff"] if get => {
            let project = app.store.get("project", query(q, "projectId"))?;
            Ok(json!({"diff":crate::workspace::review(&app,&project).await?}))
        }
        ["workspace", "undo"] if post => {
            let chat = app.store.get("chat", text(&value, "chatId"))?;
            let project = app.store.get("project", text(&chat, "projectId"))?;
            crate::workspace::undo(
                &app,
                text(&chat, "id"),
                Path::new(text(&project, "path")),
                text(&value, "path"),
            )?;
            Ok(json!({"ok":true}))
        }
        ["plugins", "install"] if post => {
            crate::plugins::install(&app, Path::new(text(&value, "path")), None)
        }
        ["plugins", "create"] if post => crate::plugins::create(&value),
        ["plugins", "build"] if post => {
            crate::plugins::build(&app, Path::new(text(&value, "path"))).await
        }
        ["plugins", plugin_id, "auth"] if post => {
            if app.store.all("run")?.iter().any(crate::tasks::active) {
                bail!("Finish or cancel active runs before changing an MCP account");
            }
            crate::mcp_auth::save_key(
                &app,
                plugin_id,
                text(&value, "serverName"),
                text(&value, "apiKey"),
            )?;
            Ok(json!({"ok":true}))
        }
        ["plugins", plugin_id, "oauth", "start"] if post => {
            crate::mcp_auth::start(app, plugin_id, text(&value, "serverName")).await
        }
        ["plugins", plugin_id, "enable"] if post => {
            let mut plugin = app.store.get("plugin", plugin_id)?;
            plugin["enabled"] = json!(value["enabled"].as_bool().unwrap_or(false));
            if plugin["enabled"] == false {
                app.cancel_plugin_runs(plugin_id)?;
            }
            app.store.put("plugin", &plugin)?;
            Ok(plugin)
        }
        ["plugins", plugin_id] if delete => {
            app.cancel_plugin_runs(plugin_id)?;
            app.store.delete("plugin", plugin_id)?;
            Ok(json!({"ok":true}))
        }
        ["plugins", plugin_id, "check"] if post => {
            let plugin = app.store.get("plugin", plugin_id)?;
            let check = crate::plugins::inspect(Path::new(text(&plugin, "path")), plugin_id)?;
            Ok(
                json!({"valid":check["diagnostics"].as_array().is_none_or(|a|a.is_empty()),"diagnostics":check["diagnostics"]}),
            )
        }
        ["plugins", plugin_id, "test"] if post => {
            let plugin = app.store.get("plugin", plugin_id)?;
            crate::plugins::test(app, &plugin).await
        }
        ["plugins", plugin_id, "pack"] if post => {
            let plugin = app.store.get("plugin", plugin_id)?;
            crate::plugins::pack(&app, &plugin, Path::new(text(&value, "outputPath")))
        }
        ["plugins", plugin_id, "update"] if post => {
            crate::plugins::install(&app, Path::new(text(&value, "path")), Some(plugin_id))
        }
        ["plugins", plugin_id, "rollback"] if post => {
            if app.store.all("run")?.iter().any(crate::tasks::active) {
                bail!("Finish active runs before rollback")
            };
            let old = app.store.get("plugin", plugin_id)?;
            let previous = text(&old, "previousPath");
            if previous.is_empty() {
                bail!("No retained version available")
            };
            let mut plugin = crate::plugins::inspect(Path::new(previous), plugin_id)?;
            plugin["source"] = old["source"].clone();
            plugin["previousPath"] = old["path"].clone();
            plugin["enabled"] = json!(false);
            app.store.put("plugin", &plugin)?;
            Ok(plugin)
        }
        ["plugins", plugin_id, "skills", skill] if get => Ok(
            json!({"content":crate::plugins::skill_content(&app.store.get("plugin",plugin_id)?,skill)?}),
        ),
        ["plugins", plugin_id, "skills", skill] if *method == Method::PUT => {
            let plugin = app.store.get("plugin", plugin_id)?;
            let source = Path::new(text(&plugin, "source"));
            if !source.is_dir() {
                bail!(
                    "This package came from a ZIP. Extract it to an authoring folder before editing."
                )
            };
            let source_plugin = crate::plugins::inspect(source, plugin_id)?;
            let record = source_plugin["skills"]
                .as_array()
                .and_then(|a| a.iter().find(|s| s["name"] == *skill))
                .context("Skill not found in author source")?;
            let file = std::fs::canonicalize(source.join(text(record, "path")))?;
            if !file.starts_with(std::fs::canonicalize(source)?) {
                bail!("Skill escapes source")
            };
            std::fs::write(file, text(&value, "content"))?;
            Ok(
                json!({"ok":true,"message":"Saved author source. Update the installed plugin to activate this version."}),
            )
        }
        ["settings"] if get => Ok(app.settings()),
        ["settings"] if post => {
            value["id"] = json!("settings");
            value["maxIterations"] =
                json!(value["maxIterations"].as_u64().unwrap_or(20).clamp(1, 50));
            value["commandTimeoutSecs"] = json!(
                value["commandTimeoutSecs"]
                    .as_u64()
                    .unwrap_or(120)
                    .clamp(1, 600)
            );
            if !["dark", "light", "system"].contains(&text(&value, "theme")) {
                bail!("Unsupported theme")
            };
            app.store.put("settings", &value)?;
            Ok(value)
        }
        ["system", "pick-directory"] if post => {
            let selected = tokio::task::spawn_blocking(|| {
                rfd::FileDialog::new()
                    .set_title("Select a Workbench folder")
                    .pick_folder()
            })
            .await?;
            Ok(json!({"path":selected.map(|p|p.to_string_lossy().to_string())}))
        }
        ["system", "open"] if post => {
            let target = if !text(&value, "url").is_empty() {
                let u = url::Url::parse(text(&value, "url"))?;
                if !["https", "http"].contains(&u.scheme()) {
                    bail!("Only HTTP(S) links can be opened")
                };
                u.to_string()
            } else {
                let p = std::fs::canonicalize(text(&value, "path"))?;
                p.to_string_lossy().to_string()
            };
            webbrowser::open(&target)?;
            Ok(json!({"ok":true}))
        }
        ["doctor"] if get => {
            let mut checks = vec![];
            for (program, args) in [
                ("git", vec!["--version".into()]),
                ("python", vec!["--version".into()]),
                ("node", vec!["--version".into()]),
                ("cargo", vec!["--version".into()]),
                (
                    "docker",
                    vec![
                        "version".into(),
                        "--format".into(),
                        "{{.Server.Version}}".into(),
                    ],
                ),
            ] {
                let result = crate::runtime::command_output(
                    &app.data_dir,
                    program,
                    &args,
                    10,
                    &tokio_util::sync::CancellationToken::new(),
                    false,
                )
                .await;
                checks.push(match result{Ok(v)=>json!({"name":program,"ok":v["exitCode"]==0,"detail":format!("{}{}",text(&v,"stdout"),text(&v,"stderr"))}),Err(e)=>json!({"name":program,"ok":false,"detail":e.to_string()})});
            }
            checks.push(
                json!({"name":"Data directory","ok":true,"detail":app.data_dir.to_string_lossy()}),
            );
            Ok(json!({"checks":checks}))
        }
        ["export"] if post => {
            let mut state = app.state()?;
            state.as_object_mut().unwrap().remove("approvals");
            state.as_object_mut().unwrap().remove("runs");
            let dir = app.data_dir.join("exports");
            std::fs::create_dir_all(&dir)?;
            let file = dir.join(format!("workbench-config-{}.json", id()));
            std::fs::write(&file, serde_json::to_string_pretty(&state)?)?;
            Ok(json!({"path":file.to_string_lossy()}))
        }
        _ => bail!("Unknown endpoint or HTTP method: {method} /api/{path}"),
    }
}
