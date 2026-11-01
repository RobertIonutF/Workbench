use anyhow::Result;
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use workbench_core::{App, app::text};

#[derive(Clone)]
struct Fixture {
    requests: Arc<AtomicUsize>,
}
async fn response(
    State(state): State<Fixture>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    state.requests.fetch_add(1, Ordering::SeqCst);
    let messages = body["messages"].as_array().unwrap();
    let user = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("");
    let calls = if user.contains("budget-loop") {
        Some(
            json!([{"index":0,"id":"read_call","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"sample.rs\"}"}}]),
        )
    } else {
        None
    };
    let delta = if let Some(calls) = calls {
        json!({"tool_calls":calls})
    } else {
        json!({"content":format!("Observed: {user}")})
    };
    let finish = if user.contains("budget-loop") {
        "tool_calls"
    } else {
        "stop"
    };
    let first = json!({"choices":[{"delta":delta,"finish_reason":null}]});
    let last = json!({"choices":[{"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":100,"completion_tokens":20}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
    )
}
async fn setup() -> Result<(tempfile::TempDir, Arc<App>, Value, Arc<AtomicUsize>)> {
    let directory = tempfile::tempdir()?;
    let project_path = directory.path().join("project");
    std::fs::create_dir(&project_path)?;
    std::fs::write(
        project_path.join("sample.rs"),
        "pub fn original() {\n    println!(\"before\");\n}\n",
    )?;
    let counter = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let state = Fixture {
        requests: counter.clone(),
    };
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(response))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    let app = App::new(Some(directory.path().join("profile"))).await?;
    let connection=app.connection(json!({"label":"Fixture","preset":"custom","baseUrl":format!("http://{address}/v1"),"protocol":"openai_chat","modelId":"custom/model@q4","tools":true,"authKind":"none","contextWindow":32768}))?;
    let project = app.project(json!({"path":project_path}))?;
    let chat=app.chat(json!({"connectionId":connection["id"],"modelId":connection["modelId"],"projectId":project["id"],"executionMode":"trusted_native"}))?;
    Ok((directory, app, chat, counter))
}
async fn finished(app: &App, run: &Value) -> Value {
    for _ in 0..300 {
        let record = app.store.get("run", text(run, "id")).unwrap();
        if !workbench_core::tasks::active(&record) {
            return record;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run did not finish")
}

#[tokio::test]
async fn model_profiles_keep_exact_ids_and_destination_scope() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    let profile = workbench_core::models::save(
        &app,
        json!({"label":"Local quantized","connectionId":chat["connectionId"],"modelId":"odd/model:Q4@instance-2","contextWindow":8192,"maxOutputTokens":256,"tools":true,"temperature":0.2,"inputPrice":0.0,"outputPrice":0.0}),
    )?;
    let effective = workbench_core::models::effective(
        &app,
        &json!({"connectionId":chat["connectionId"],"modelId":profile["modelId"],"profileId":profile["id"]}),
    )?;
    assert_eq!(effective["maxOutputTokens"], 256);
    assert_eq!(profile["modelId"], "odd/model:Q4@instance-2");
    let mut connection = app.store.get("connection", text(&chat, "connectionId"))?;
    connection["baseUrl"] = json!("http://127.0.0.1:19999/v1");
    app.connection(connection)?;
    assert!(workbench_core::models::effective(&app,&json!({"connectionId":chat["connectionId"],"modelId":profile["modelId"],"profileId":profile["id"]})).is_err());
    Ok(())
}
#[tokio::test]
async fn request_budget_stops_before_a_second_provider_call() -> Result<()> {
    let (_directory, app, mut chat, counter) = setup().await?;
    chat["budget"] = json!({"maxRequests":1,"maxTokens":100000});
    app.chat(chat.clone())?;
    let run = workbench_core::runtime::start(app.clone(), text(&chat, "id"), "budget-loop")?;
    let done = finished(&app, &run).await;
    assert_eq!(done["status"], "failed");
    assert!(text(&done, "error").contains("budget"));
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(app.store.all("usage")?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn pause_and_steering_are_durable_at_model_boundaries() -> Result<()> {
    let (_directory, app, chat, counter) = setup().await?;
    let run = workbench_core::runtime::start(app.clone(), text(&chat, "id"), "original request")?;
    workbench_core::tasks::control(&app, text(&run, "id"), "pause")?;
    for _ in 0..100 {
        if app.store.get("run", text(&run, "id"))?["status"] == "paused" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(app.store.get("run", text(&run, "id"))?["status"], "paused");
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    workbench_core::tasks::enqueue(
        &app,
        text(&chat, "id"),
        &json!({"mode":"steer","text":"revised instruction"}),
    )?;
    workbench_core::tasks::control(&app, text(&run, "id"), "resume")?;
    assert_eq!(finished(&app, &run).await["status"], "completed");
    assert!(app.store.messages(text(&chat,"id"))?.iter().any(|m|m["role"]=="assistant"&&text(m,"content").contains("revised instruction")));
    Ok(())
}
#[tokio::test]
async fn context_index_memory_and_scope_have_real_provenance() -> Result<()> {
    let (directory, app, mut chat, _) = setup().await?;
    let index = workbench_core::context::index(&app, text(&chat, "projectId"), "original")?;
    assert_eq!(index["symbols"][0]["name"], "original");
    let item = workbench_core::context::save(
        &app,
        json!({"kind":"file","name":"code","projectId":chat["projectId"],"path":"sample.rs"}),
    )?;
    workbench_core::tasks::save_memory(
        &app,
        &json!({"projectId":chat["projectId"],"content":"Use cargo checks before delivery","source":"user decision"}),
    )?;
    chat["contextIds"] = json!([item["id"]]);
    let (content, _, provenance) = workbench_core::context::expand(&app, &chat)?;
    assert!(content.contains("pub fn original"));
    assert!(content.contains("cargo checks"));
    assert_eq!(provenance["sources"].as_array().unwrap().len(), 1);
    assert!(
        workbench_core::context::save(
            &app,
            json!({"kind":"file","projectId":chat["projectId"],"path":"../outside"})
        )
        .is_err()
    );
    let other = app.project(json!({"path":directory.path()}))?;
    chat["projectId"] = other["id"].clone();
    assert!(workbench_core::context::expand(&app, &chat).is_err());
    Ok(())
}
#[tokio::test]
async fn selecting_a_nonvision_model_cannot_leave_a_phantom_run() -> Result<()> {
    use base64::Engine;
    let (_directory, app, mut chat, _) = setup().await?;
    let item = workbench_core::context::save(
        &app,
        json!({"kind":"image","name":"fixture","dataBase64":base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture")}),
    )?;
    chat["contextIds"] = json!([item["id"]]);
    app.chat(chat.clone())?;
    assert!(
        workbench_core::runtime::start(app.clone(), text(&chat, "id"), "Inspect this image")
            .is_err()
    );
    assert!(app.store.all("run")?.is_empty());
    assert!(app.runs.lock().is_empty());
    Ok(())
}
#[tokio::test]
async fn hunk_rejection_preserves_the_other_change_and_checks_staleness() -> Result<()> {
    let (directory, app, chat, _) = setup().await?;
    let project = directory.path().join("project");
    let before = (0..30).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(project.join("review.txt"), &before)?;
    let after = before
        .replace("line 2\n", "changed first\n")
        .replace("line 25\n", "changed second\n");
    workbench_core::workspace::write(
        &app,
        text(&chat, "id"),
        &project,
        "review.txt",
        &after,
        Some(&before),
    )?;
    let review = workbench_core::worktrees::review(&app, text(&chat, "id"), "review.txt")?;
    assert_eq!(review["hunks"].as_array().unwrap().len(), 2);
    workbench_core::worktrees::decide_hunk(
        &app,
        &json!({"chatId":chat["id"],"path":"review.txt","hash":review["hash"],"hunkId":0,"decision":"reject"}),
    )?;
    let actual = std::fs::read_to_string(project.join("review.txt"))?;
    assert!(actual.contains("line 2\n"));
    assert!(actual.contains("changed second\n"));
    assert!(workbench_core::worktrees::decide_hunk(&app,&json!({"chatId":chat["id"],"path":"review.txt","hash":review["hash"],"hunkId":1,"decision":"reject"})).is_err());
    Ok(())
}
fn git_initial(project: &std::path::Path) -> Result<()> {
    let repo = git2::Repository::init(project)?;
    let mut index = repo.index()?;
    index.add_path(std::path::Path::new("sample.rs"))?;
    index.write()?;
    let tree = repo.find_tree(index.write_tree()?)?;
    let signature = git2::Signature::now("Fixture", "fixture@example.invalid")?;
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])?;
    Ok(())
}
#[tokio::test]
async fn worktree_commit_and_integration_preserve_original_until_review() -> Result<()> {
    let (directory, app, chat, _) = setup().await?;
    let source = directory.path().join("project");
    git_initial(&source)?;
    let worktree =
        workbench_core::worktrees::create(&app, &json!({"projectId":chat["projectId"]}))?;
    let file = std::path::Path::new(text(&worktree, "path")).join("sample.rs");
    std::fs::write(&file, "pub fn changed() {}\n")?;
    assert!(std::fs::read_to_string(source.join("sample.rs"))?.contains("original"));
    workbench_core::worktrees::commit(
        &app,
        &json!({"projectId":worktree["projectId"],"paths":["sample.rs"],"message":"actual change"}),
    )?;
    workbench_core::worktrees::merge(&app, text(&worktree, "id"))?;
    assert!(std::fs::read_to_string(source.join("sample.rs"))?.contains("changed"));
    Ok(())
}
#[tokio::test]
async fn failing_completion_hook_prevents_a_successful_result() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    workbench_core::tasks::save_hook(
        &app,
        &json!({"projectId":chat["projectId"],"label":"Failing check","event":"before_complete","program":"python","args":["-c","import sys;sys.exit(7)"],"enabled":true,"confirmNative":true}),
    )?;
    let run = workbench_core::runtime::start(app.clone(), text(&chat, "id"), "Complete this task")?;
    let done = finished(&app, &run).await;
    assert_eq!(done["status"], "failed");
    assert!(text(&done, "error").contains("Failing check"));
    Ok(())
}
#[tokio::test]
async fn scheduled_tasks_create_distinct_chats_and_one_completion_notice() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    let mut schedule = workbench_core::tasks::save_schedule(
        &app,
        &json!({"name":"Fixture schedule","chatId":chat["id"],"prompt":"scheduled fixture","intervalMinutes":60,"enabled":true,"confirmExecution":true,"notify":true}),
    )?;
    schedule["nextRunAt"] = json!(chrono::Utc::now().to_rfc3339());
    app.store.put("schedule", &schedule)?;
    workbench_core::tasks::tick(app.clone()).await?;
    let schedule = app.store.get("schedule", text(&schedule, "id"))?;
    let run = app.store.get("run", text(&schedule, "lastRunId"))?;
    assert_ne!(run["chatId"], chat["id"]);
    assert_eq!(finished(&app, &run).await["status"], "completed");
    workbench_core::tasks::tick(app.clone()).await?;
    workbench_core::tasks::tick(app.clone()).await?;
    assert_eq!(app.store.all("notification")?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn comparisons_report_real_responses_and_assertions() -> Result<()> {
    let (_directory, app, chat, counter) = setup().await?;
    let profile = workbench_core::models::save(
        &app,
        json!({"label":"Comparison","connectionId":chat["connectionId"],"modelId":"custom/model@q4","contextWindow":8192,"maxOutputTokens":256}),
    )?;
    let comparison = workbench_core::models::compare(
        app.clone(),
        json!({"profileIds":[profile["id"]],"prompt":"comparison evidence","expectedText":"Observed: comparison"}),
    )?;
    for _ in 0..200 {
        if app.store.get("comparison", text(&comparison, "id"))?["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let result = app.store.get("comparison", text(&comparison, "id"))?;
    assert_eq!(result["status"], "completed");
    assert_eq!(result["results"][0]["passed"], true);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    Ok(())
}
#[tokio::test]
async fn interactive_pty_roundtrip_executes_real_input_and_can_stop() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    let record = workbench_core::terminal::start(
        app.clone(),
        json!({"projectId":chat["projectId"],"program":"python","args":["-u","-c","print('PTY_READY',flush=True);value=input();print('PTY_ECHO:'+value,flush=True)"],"confirmed":true}),
    )?;
    let identity = text(&record, "id");
    let mut output = String::new();
    let mut cursor = 0;
    for _ in 0..100 {
        let result = workbench_core::terminal::read(&app, identity, cursor)?;
        cursor = result["cursor"].as_u64().unwrap();
        output.push_str(text(&result, "text"));
        if output.contains("\x1b[6n") {
            workbench_core::terminal::input(&app, identity, &json!({"text":"\x1b[1;1R"}))?;
            output = output.replace("\x1b[6n", "");
        }
        if output.contains("PTY_READY") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(output.contains("PTY_READY"), "{output}");
    workbench_core::terminal::input(&app, identity, &json!({"text":"terminal-value\r"}))?;
    for _ in 0..100 {
        let result = workbench_core::terminal::read(&app, identity, cursor)?;
        cursor = result["cursor"].as_u64().unwrap();
        output.push_str(text(&result, "text"));
        if output.contains("PTY_ECHO:terminal-value") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    workbench_core::terminal::stop(&app, identity)?;
    assert!(output.contains("PTY_ECHO:terminal-value"), "{output}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}

#[tokio::test]
async fn a_shared_team_budget_cannot_be_expanded_by_worker_minimums() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    let profile = workbench_core::models::save(
        &app,
        json!({"label":"Worker","connectionId":chat["connectionId"],"modelId":"custom/model@q4"}),
    )?;
    let result = workbench_core::tasks::team(
        app.clone(),
        json!({"projectId":chat["projectId"],"prompt":"shared cap","budget":{"maxRequests":1,"maxTokens":1000},"stages":[{"role":"plan","profileId":profile["id"]},{"role":"review","profileId":profile["id"]}]}),
    );
    assert!(result.is_err());
    assert!(app.store.all("team")?.is_empty());
    Ok(())
}
#[tokio::test]
async fn concurrent_scheduler_ticks_claim_only_one_due_occurrence() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    let mut schedule = workbench_core::tasks::save_schedule(
        &app,
        &json!({"name":"Once","chatId":chat["id"],"prompt":"one occurrence","enabled":true,"confirmExecution":true}),
    )?;
    schedule["nextRunAt"] = json!(chrono::Utc::now().to_rfc3339());
    app.store.put("schedule", &schedule)?;
    let (a, b) = tokio::join!(
        workbench_core::tasks::tick(app.clone()),
        workbench_core::tasks::tick(app.clone())
    );
    a?;
    b?;
    let runs: Vec<Value> = app.store.all("run")?;
    assert_eq!(runs.len(), 1);
    finished(&app, &runs[0]).await;
    Ok(())
}
#[tokio::test]
async fn a_hook_removed_during_approval_cannot_execute() -> Result<()> {
    let (directory, app, mut chat, _) = setup().await?;
    chat["executionMode"] = json!("ask");
    app.chat(chat.clone())?;
    let marker = directory.path().join("project/should-not-exist.txt");
    let hook = workbench_core::tasks::save_hook(
        &app,
        &json!({"projectId":chat["projectId"],"label":"Revocable","event":"before_complete","program":"python","args":["-c","open('should-not-exist.txt','w').write('bad')"],"enabled":true,"confirmNative":true}),
    )?;
    let run = workbench_core::runtime::start(app.clone(), text(&chat, "id"), "finish")?;
    let mut approval = None;
    for _ in 0..200 {
        approval = app
            .store
            .all("approval")?
            .into_iter()
            .find(|a| a["status"] == "pending");
        if approval.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let approval = approval.expect("hook must request approval");
    app.store.delete("hook", text(&hook, "id"))?;
    app.approvals
        .lock()
        .remove(text(&approval, "id"))
        .unwrap()
        .send(true)
        .unwrap();
    assert_eq!(finished(&app, &run).await["status"], "failed");
    assert!(!marker.exists());
    Ok(())
}
#[tokio::test]
async fn worktree_integration_rejects_uncommitted_source_edits() -> Result<()> {
    let (directory, app, chat, _) = setup().await?;
    git_initial(&directory.path().join("project"))?;
    let worktree =
        workbench_core::worktrees::create(&app, &json!({"projectId":chat["projectId"]}))?;
    std::fs::write(
        std::path::Path::new(text(&worktree, "path")).join("sample.rs"),
        "uncommitted work",
    )?;
    assert!(workbench_core::worktrees::merge(&app, text(&worktree, "id")).is_err());
    assert!(
        std::fs::read_to_string(directory.path().join("project/sample.rs"))?.contains("original")
    );
    Ok(())
}
#[tokio::test]
async fn queued_claims_are_not_replayed_after_recovery() -> Result<()> {
    let (directory, app, chat, _) = setup().await?;
    let mut item =
        workbench_core::tasks::enqueue(&app, text(&chat, "id"), &json!({"text":"do not replay"}))?;
    item["status"] = json!("starting");
    app.store.put("queued_turn", &item)?;
    drop(app);
    let restored = App::new(Some(directory.path().join("profile"))).await?;
    assert_eq!(
        restored.store.get("queued_turn", text(&item, "id"))?["status"],
        "interrupted"
    );
    workbench_core::tasks::tick(restored.clone()).await?;
    assert!(restored.store.all("run")?.is_empty());
    Ok(())
}
#[test]
fn thinking_and_cache_tokens_are_counted_in_provider_usage() {
    assert_eq!(
        workbench_core::models::usage_tokens(
            &json!({"promptTokenCount":100,"candidatesTokenCount":20,"thoughtsTokenCount":30})
        ),
        (100, 50, true)
    );
}

#[tokio::test]
async fn credentials_remain_bound_when_an_old_request_snapshot_survives_an_edit() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    let mut original = app.store.get("connection", text(&chat, "connectionId"))?;
    original["apiKey"] = json!("fixture-original-private-value");
    original = app.connection(original)?;
    assert_eq!(
        workbench_core::auth::api_key(&app, &original)?,
        "fixture-original-private-value"
    );
    let mut edited = original.clone();
    edited["baseUrl"] = json!("http://127.0.0.1:1/v1");
    edited["apiKey"] = json!("fixture-new-private-value");
    let edited = app.connection(edited)?;
    assert_eq!(
        workbench_core::auth::api_key(&app, &edited)?,
        "fixture-new-private-value"
    );
    assert!(workbench_core::auth::api_key(&app, &original).is_err());
    Ok(())
}
#[tokio::test]
async fn inherited_hooks_execute_in_the_worktree_and_read_only_workers_skip_them() -> Result<()> {
    let (directory, app, chat, _) = setup().await?;
    git_initial(&directory.path().join("project"))?;
    workbench_core::tasks::save_hook(
        &app,
        &json!({"projectId":chat["projectId"],"label":"Worktree evidence","event":"before_turn","program":"python","args":["-c","from pathlib import Path; Path('hook-evidence.txt').write_text('executed')"],"enabled":true,"confirmNative":true}),
    )?;
    let worktree =
        workbench_core::worktrees::create(&app, &json!({"projectId":chat["projectId"]}))?;
    let mut worker = chat.clone();
    worker["id"] = json!(workbench_core::app::id());
    worker["projectId"] = worktree["projectId"].clone();
    let worker = app.chat(worker)?;
    let run =
        workbench_core::runtime::start(app.clone(), text(&worker, "id"), "Check the worktree")?;
    assert_eq!(finished(&app, &run).await["status"], "completed");
    assert!(
        std::path::Path::new(text(&worktree, "path"))
            .join("hook-evidence.txt")
            .exists()
    );
    assert!(!directory.path().join("project/hook-evidence.txt").exists());
    let mut reader = chat.clone();
    reader["id"] = json!(workbench_core::app::id());
    reader["executionMode"] = json!("read_only");
    let reader = app.chat(reader)?;
    let run = workbench_core::runtime::start(app.clone(), text(&reader, "id"), "Read project")?;
    assert_eq!(finished(&app, &run).await["status"], "completed");
    assert!(!directory.path().join("project/hook-evidence.txt").exists());
    Ok(())
}
#[tokio::test]
async fn shutdown_cannot_start_queued_or_direct_runs() -> Result<()> {
    let (_directory, app, chat, counter) = setup().await?;
    workbench_core::tasks::enqueue(&app, text(&chat, "id"), &json!({"text":"must stay queued"}))?;
    app.begin_shutdown();
    workbench_core::tasks::tick(app.clone()).await?;
    assert!(
        workbench_core::runtime::start(app.clone(), text(&chat, "id"), "must not start").is_err()
    );
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert!(app.store.all("run")?.is_empty());
    Ok(())
}
#[tokio::test]
async fn interrupted_schedules_are_disabled_until_review() -> Result<()> {
    let (_directory, app, chat, _) = setup().await?;
    app.store.put(
        "run",
        &json!({"id":"interrupted-run","chatId":chat["id"],"status":"interrupted"}),
    )?;
    let mut schedule = workbench_core::tasks::save_schedule(
        &app,
        &json!({"chatId":chat["id"],"prompt":"review first","enabled":true,"confirmExecution":true}),
    )?;
    schedule["lastRunId"] = json!("interrupted-run");
    app.store.put("schedule", &schedule)?;
    workbench_core::features::recover(&app)?;
    assert_eq!(
        app.store.get("schedule", text(&schedule, "id"))?["enabled"],
        false
    );
    Ok(())
}
