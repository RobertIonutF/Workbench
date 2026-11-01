use anyhow::{Context, Result};
use axum::{
    Json, Router,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use workbench_core::{
    App,
    providers::{self, Message},
};

fn stream(events: Vec<Value>) -> impl IntoResponse {
    let mut body = events
        .into_iter()
        .map(|v| format!("data: {v}\n\n"))
        .collect::<String>();
    body.push_str("data: [DONE]\n\n");
    ([("content-type", "text/event-stream")], body)
}
async fn mock_chat(Json(body): Json<Value>) -> impl IntoResponse {
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    if last["role"] == "tool" {
        return stream(vec![
            json!({"choices":[{"delta":{"content":"The tool result was received and checked."},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":42}}),
        ]);
    }
    let user = last["content"].as_str().unwrap_or("");
    let tool = if user.contains("connection_echo") {
        Some(("connection_echo", json!({"text":"verified"})))
    } else if user.contains("patch") {
        Some((
            "apply_patch",
            json!({"path":"sample.txt","oldText":"before","newText":"after"}),
        ))
    } else if user.contains("malformed") {
        Some(("write_file", json!({"path":"sample.txt"})))
    } else {
        None
    };
    if let Some((name, args)) = tool {
        return stream(vec![
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_fixture","type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]);
    }
    stream(vec![
        json!({"choices":[{"delta":{"content":"Workbench connection verified."},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ])
}
async fn mock() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let router = Router::new()
            .route(
                "/v1/models",
                get(|| async { Json(json!({"data":[{"id":"fixture/model@q4"}]})) }),
            )
            .route("/v1/chat/completions", post(mock_chat));
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}/v1")
}
async fn setup(mode: &str) -> Result<(tempfile::TempDir, Arc<App>, Value)> {
    let d = tempfile::tempdir()?;
    std::fs::create_dir(d.path().join("project"))?;
    std::fs::write(d.path().join("project/sample.txt"), "before")?;
    let app = App::new(Some(d.path().join("data"))).await?;
    let conn=app.connection(json!({"label":"Fixture only","preset":"custom","protocol":"openai_chat","baseUrl":mock().await,"modelId":"fixture/model@q4","contextWindow":8192,"tools":true,"authKind":"none"}))?;
    let project = app.project(json!({"path":d.path().join("project")}))?;
    let chat=app.chat(json!({"connectionId":conn["id"],"modelId":conn["modelId"],"projectId":project["id"],"executionMode":mode}))?;
    Ok((d, app, chat))
}
async fn wait_run(app: &App, id: &str) -> Value {
    for _ in 0..200 {
        let run = app.store.get("run", id).unwrap();
        if run["status"] != "running" {
            return run;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run did not finish")
}

#[tokio::test]
async fn cli_reuses_the_locked_desktop_runtime_without_recovering_live_runs() -> Result<()> {
    let (directory, app, _) = setup("ask").await?;
    let (address, _) = workbench_core::api::start(app.clone(), 0, None).await?;
    let info = workbench_core::auth::runtime_info(Some(directory.path().join("data")))?;
    assert_eq!(info["baseUrl"], format!("http://{address}"));
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_workbench-cli"))
        .arg("--data-dir")
        .arg(directory.path().join("data"))
        .args(["plugin", "list"])
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plugins: Value = serde_json::from_slice(&output.stdout)?;
    assert!(plugins.is_array());
    let connection = app.store.all("connection")?.remove(0);
    let chat_output = tokio::process::Command::new(env!("CARGO_BIN_EXE_workbench-cli"))
        .arg("--data-dir")
        .arg(directory.path().join("data"))
        .args([
            "chat",
            "Hello from the CLI",
            "--read-only",
            "--connection",
            connection["id"].as_str().unwrap(),
        ])
        .output()
        .await?;
    assert!(
        chat_output.status.success(),
        "{}",
        String::from_utf8_lossy(&chat_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&chat_output.stdout).contains("Workbench connection verified.")
    );
    assert!(App::new(Some(directory.path().join("data"))).await.is_err());
    Ok(())
}

#[tokio::test]
async fn ordinary_folder_review_shows_real_checkpoint_changes() -> Result<()> {
    let (directory, app, chat) = setup("ask").await?;
    let project = app
        .store
        .get("project", chat["projectId"].as_str().unwrap())?;
    workbench_core::workspace::write(
        &app,
        chat["id"].as_str().unwrap(),
        &directory.path().join("project"),
        "sample.txt",
        "after\n",
        Some("before"),
    )?;
    let diff = workbench_core::workspace::review(&app, &project).await?;
    assert!(diff.contains("-before"), "{diff}");
    assert!(diff.contains("+after"), "{diff}");
    Ok(())
}

#[tokio::test]
async fn agent_approval_patch_and_persistence() -> Result<()> {
    let (d, app, chat) = setup("ask").await?;
    let run = workbench_core::runtime::start(
        app.clone(),
        chat["id"].as_str().unwrap(),
        "Please patch sample.txt",
    )?;
    let mut pending = None;
    for _ in 0..200 {
        pending = app
            .store
            .all("approval")?
            .into_iter()
            .find(|a| a["status"] == "pending");
        if pending.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let approval = pending.expect("mutation must request approval");
    assert_eq!(
        std::fs::read_to_string(d.path().join("project/sample.txt"))?,
        "before"
    );
    let sender = app
        .approvals
        .lock()
        .remove(approval["id"].as_str().unwrap())
        .unwrap();
    sender.send(true).unwrap();
    let done = wait_run(&app, run["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(
        std::fs::read_to_string(d.path().join("project/sample.txt"))?,
        "after"
    );
    let chat_id = chat["id"].as_str().unwrap();
    assert_eq!(app.store.messages(chat_id)?.len(), 4);
    drop(app);
    let restored = App::new(Some(d.path().join("data"))).await?;
    assert_eq!(restored.store.messages(chat_id)?.len(), 4);
    Ok(())
}
#[tokio::test]
async fn read_only_and_invalid_arguments_never_write() -> Result<()> {
    for (mode, prompt) in [
        ("read_only", "patch sample.txt"),
        ("trusted_native", "malformed write"),
    ] {
        let (d, app, chat) = setup(mode).await?;
        let run =
            workbench_core::runtime::start(app.clone(), chat["id"].as_str().unwrap(), prompt)?;
        let done = wait_run(&app, run["id"].as_str().unwrap()).await;
        assert_eq!(done["status"], "completed", "{done}");
        assert_eq!(
            std::fs::read_to_string(d.path().join("project/sample.txt"))?,
            "before"
        );
        assert!(
            app.store
                .messages(chat["id"].as_str().unwrap())?
                .iter()
                .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("error"))
        );
    }
    Ok(())
}
#[tokio::test]
async fn denied_approval_does_not_write() -> Result<()> {
    let (d, app, chat) = setup("ask").await?;
    let run = workbench_core::runtime::start(
        app.clone(),
        chat["id"].as_str().unwrap(),
        "patch sample.txt",
    )?;
    for _ in 0..200 {
        let approval = app
            .store
            .all("approval")?
            .into_iter()
            .find(|a| a["status"] == "pending");
        if let Some(a) = approval {
            app.approvals
                .lock()
                .remove(a["id"].as_str().unwrap())
                .unwrap()
                .send(false)
                .unwrap();
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    wait_run(&app, run["id"].as_str().unwrap()).await;
    assert_eq!(
        std::fs::read_to_string(d.path().join("project/sample.txt"))?,
        "before"
    );
    Ok(())
}
#[tokio::test]
async fn authenticated_api_and_catalog_preserve_ids() -> Result<()> {
    let (_d, app, _) = setup("ask").await?;
    let (address, token) = workbench_core::api::start(app.clone(), 0, None).await?;
    let client = reqwest::Client::new();
    let url = format!("http://{address}/api/state");
    assert_eq!(client.get(&url).send().await?.status(), 401);
    assert_eq!(
        client
            .get(&url)
            .header("X-Workbench-Token", &token)
            .header("Origin", "https://untrusted.invalid")
            .send()
            .await?
            .status(),
        403
    );
    let state: Value = client
        .get(&url)
        .header("X-Workbench-Token", &token)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(state["connections"].as_array().unwrap().len(), 1);
    let conn = &app.store.all("connection")?[0];
    let models = providers::models(&app, conn).await?;
    assert_eq!(models["models"][0]["id"], "fixture/model@q4");
    let probe = providers::probe(&app, conn, "fixture/model@q4").await?;
    assert_eq!(probe["tools"], true);
    Ok(())
}
#[tokio::test]
async fn generated_python_plugin_real_mcp_roundtrip() -> Result<()> {
    let d = tempfile::tempdir()?;
    let source = d.path().join("plugin");
    workbench_core::plugins::create(
        &json!({"path":source,"name":"calculator-test","description":"Add numbers","template":"combined","language":"python"}),
    )?;
    let app = App::new(Some(d.path().join("data"))).await?;
    let plugin = workbench_core::plugins::install(&app, &source, None)?;
    let cancel = CancellationToken::new();
    let server = &plugin["servers"][0];
    let mut session = workbench_core::mcp::Session::connect(
        server,
        std::path::Path::new(plugin["path"].as_str().unwrap()),
        &d.path().join("mcp-data"),
        app.client.clone(),
        &cancel,
    )
    .await?;
    let tools = session.tools(&cancel).await?;
    assert_eq!(tools[0]["name"], "calculate");
    let result = session
        .call(
            "tools/call",
            json!({"name":"calculate","arguments":{"numbers":[40,2]}}),
            &cancel,
        )
        .await?;
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap())?["sum"],
        42
    );
    session.close().await;
    let zipped = workbench_core::plugins::pack(&app, &plugin, &d.path().join("plugin.zip"))?;
    let imported = workbench_core::plugins::install(
        &app,
        std::path::Path::new(zipped["path"].as_str().unwrap()),
        None,
    )?;
    assert_eq!(imported["skills"].as_array().unwrap().len(), 1);
    Ok(())
}
#[tokio::test]
async fn canceled_command_stops_quickly() -> Result<()> {
    let d = tempfile::tempdir()?;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        stop.cancel();
    });
    let start = std::time::Instant::now();
    let result = workbench_core::runtime::command_output(
        d.path(),
        "python",
        &["-c".into(), "import time;time.sleep(60)".into()],
        90,
        &cancel,
        false,
    )
    .await;
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(5));
    Ok(())
}
#[tokio::test]
async fn partial_stream_is_not_success() -> Result<()> {
    let d = tempfile::tempdir()?;
    let app = App::new(Some(d.path().into())).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/chat/completions",
                post(|| async {
                    (
                        [("content-type", "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                    )
                }),
            ),
        )
        .await
        .unwrap();
    });
    let c =
        json!({"protocol":"openai_chat","baseUrl":format!("http://{address}"),"authKind":"none"});
    let result = providers::generate(
        &app,
        &c,
        "model",
        "system",
        &[Message::new("user", "hello".into())],
        &[],
        &CancellationToken::new(),
        |_| {},
    )
    .await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn interrupted_tools_reconcile_without_replay() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let app = App::new(Some(directory.path().into())).await?;
    app.store.put(
        "run",
        &json!({"id":"interrupted","chatId":"history","status":"running"}),
    )?;
    let mut assistant = Message::new("assistant", "Beginning requested action".into());
    assistant.tool_calls.push(providers::ToolCall {
        id: "pending-action".into(),
        name: "run_command".into(),
        arguments: json!({"program":"python","args":["-c","raise Exception('must not execute')"]})
            .to_string(),
    });
    app.add_message(
        "history",
        "assistant",
        "Beginning requested action",
        json!({"canonical":assistant,"connectionId":"connection","modelId":"model"}),
    )?;
    drop(app);
    let restored = App::new(Some(directory.path().into())).await?;
    assert_eq!(
        restored.store.get("run", "interrupted")?["status"],
        "interrupted"
    );
    let messages = restored.store.messages("history")?;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["canonical"]["tool_call_id"], "pending-action");
    assert!(messages[1]["content"].as_str().unwrap().contains("unknown"));
    assert!(restored.runs.lock().is_empty());
    Ok(())
}

#[tokio::test]
async fn lm_catalog_uses_loaded_context_and_filters_embedding_models() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let app = App::new(Some(directory.path().into())).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let router=Router::new()
            .route("/v1/models",get(||async{Json(json!({"data":[{"id":"alias/coder@q4"},{"id":"embedding-model"},{"id":"unloaded-model"}]}))}))
            .route("/api/v1/models",get(||async{Json(json!({"models":[
                {"type":"llm","key":"downloaded-model-key","max_context_length":131072,"loaded_instances":[{"id":"alias/coder@q4","config":{"context_length":4096}}]},
                {"type":"embedding","key":"embedding-model","loaded_instances":[]},
                {"type":"llm","key":"unloaded-model","loaded_instances":[]}
            ]}))}));
        axum::serve(listener, router).await.unwrap();
    });
    let connection=app.connection(json!({"label":"LM fixture","preset":"lmstudio","protocol":"openai_chat","baseUrl":format!("http://{address}/v1"),"authKind":"none"}))?;
    let result = providers::models(&app, &connection).await?;
    let items = result["models"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "alias/coder@q4");
    assert_eq!(items[0]["contextWindow"], 4096);
    assert_eq!(items[0]["modelKey"], "downloaded-model-key");
    assert_eq!(items[1]["loaded"], false);
    Ok(())
}

#[tokio::test]
async fn second_runtime_cannot_invalidate_live_runs() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let app = App::new(Some(directory.path().into())).await?;
    app.store.put(
        "run",
        &json!({"id":"live","status":"running","chatId":"chat"}),
    )?;
    assert!(App::new(Some(directory.path().into())).await.is_err());
    assert_eq!(app.store.get("run", "live")?["status"], "running");
    Ok(())
}

#[tokio::test]
async fn git_inspection_does_not_execute_repository_helpers() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let repository = git2::Repository::init(directory.path())?;
    let marker = directory.path().join("helper-executed");
    let helper = if cfg!(windows) {
        format!("cmd /C echo invoked > \"{}\"", marker.display())
    } else {
        format!("sh -c 'echo invoked > {}'", marker.display())
    };
    let mut config = repository.config()?;
    for setting in [
        "core.fsmonitor",
        "filter.hostile.clean",
        "filter.hostile.smudge",
        "filter.hostile.process",
        "diff.hostile.textconv",
        "diff.external",
    ] {
        config.set_str(setting, &helper)?;
    }
    std::fs::write(
        directory.path().join(".gitattributes"),
        "tracked.txt filter=hostile diff=hostile\n",
    )?;
    std::fs::write(directory.path().join("tracked.txt"), "before\n")?;
    let mut index = repository.index()?;
    index.add_path(std::path::Path::new("tracked.txt"))?;
    index.write()?;
    std::fs::write(directory.path().join("tracked.txt"), "after\n")?;
    let status = workbench_core::workspace::status(directory.path())?;
    let diff = workbench_core::workspace::diff(directory.path()).await?;
    assert!(!status["entries"].as_array().unwrap().is_empty());
    assert!(diff.contains("+after"));
    assert!(
        !marker.exists(),
        "read-only Git inspection executed a configured helper"
    );
    Ok(())
}

#[tokio::test]
async fn disabling_plugin_during_launch_approval_prevents_process_start() -> Result<()> {
    let (directory, app, chat) = setup("ask").await?;
    let source = directory.path().join("plugin");
    workbench_core::plugins::create(
        &json!({"path":source,"name":"revocation-fixture","template":"combined","language":"python"}),
    )?;
    let marker = directory.path().join("server-started");
    let script = source.join("server/server.py");
    let old = std::fs::read_to_string(&script)?;
    std::fs::write(
        &script,
        format!(
            "import pathlib\npathlib.Path({}).write_text('started')\n{old}",
            serde_json::to_string(&marker.to_string_lossy())?
        ),
    )?;
    let mut plugin = workbench_core::plugins::install(&app, &source, None)?;
    let mut configured = chat.clone();
    configured["pluginIds"] = json!([plugin["id"]]);
    app.chat(configured)?;
    let run = workbench_core::runtime::start(
        app.clone(),
        chat["id"].as_str().unwrap(),
        "Use the selected plugin",
    )?;
    let mut approval = None;
    for _ in 0..200 {
        approval = app
            .store
            .all("approval")?
            .into_iter()
            .find(|a| a["runId"] == run["id"] && a["status"] == "pending");
        if approval.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let approval = approval.context("launch must require approval")?;
    plugin["enabled"] = json!(false);
    app.store.put("plugin", &plugin)?;
    app.approvals
        .lock()
        .remove(approval["id"].as_str().unwrap())
        .unwrap()
        .send(true)
        .unwrap();
    let done = wait_run(&app, run["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "failed");
    assert!(
        !marker.exists(),
        "revoked plugin process was launched after approval"
    );
    Ok(())
}
