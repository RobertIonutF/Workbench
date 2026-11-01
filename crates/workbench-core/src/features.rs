use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Result, bail};
use axum::http::Method;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub fn recover(app: &App) -> Result<()> {
    for mut schedule in app.store.all("schedule")? {
        if app
            .store
            .get("run", text(&schedule, "lastRunId"))
            .ok()
            .is_some_and(|r| r["status"] == "interrupted")
        {
            schedule["enabled"] = json!(false);
            schedule["lastStatus"] = json!("interrupted");
            schedule["error"] =
                json!("Review the interrupted run before enabling this schedule again");
            app.store.put("schedule", &schedule)?;
        }
    }
    for kind in ["team", "comparison", "preview", "terminal", "queued_turn"] {
        for mut item in app.store.all(kind)? {
            if item["status"] == "running" || item["status"] == "starting" {
                item["status"] = json!("interrupted");
                item["error"] =
                    json!("The app stopped; no external action was automatically replayed");
                app.store.put(kind, &item)?;
            }
        }
    }
    Ok(())
}
pub fn start_services(app: Arc<App>) {
    if app.services_started.swap(true, Ordering::SeqCst) {
        return;
    }
    let weak = Arc::downgrade(&app);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(app) = weak.upgrade() else {
                break;
            };
            if app.shutting_down.load(Ordering::SeqCst) {
                break;
            }
            if let Err(error) = crate::tasks::tick(app).await {
                tracing::warn!("Task scheduler: {error}");
            }
        }
    });
}
pub async fn dispatch(
    app: Arc<App>,
    method: &Method,
    path: &str,
    q: &HashMap<String, String>,
    value: Value,
) -> Result<Value> {
    let parts: Vec<&str> = path.trim_start_matches("features/").split('/').collect();
    let post = *method == Method::POST;
    let get = *method == Method::GET;
    let delete = *method == Method::DELETE;
    match parts.as_slice() {
        ["state"] if get => app.state(),
        ["terminal", "start"] if post => {
            tokio::task::spawn_blocking(move || crate::terminal::start(app, value)).await?
        }
        ["terminal", identity, "read"] if get => crate::terminal::read(
            &app,
            identity,
            q.get("cursor").and_then(|v| v.parse().ok()).unwrap_or(0),
        ),
        ["terminal", identity, "input"] if post => crate::terminal::input(&app, identity, &value),
        ["terminal", identity, "resize"] if post => crate::terminal::resize(&app, identity, &value),
        ["terminal", identity, "stop"] if post => crate::terminal::stop(&app, identity),
        ["profiles"] if post => crate::models::save(&app, value),
        ["profiles", identity, "probe"] if post => crate::models::probe(app, identity).await,
        ["profiles", identity] if delete => {
            if app
                .store
                .all("chat")?
                .iter()
                .any(|c| c["profileId"] == *identity)
            {
                bail!("Unselect this profile from its tasks before deleting it");
            }
            app.store.delete("profile", identity)?;
            Ok(json!({"ok":true}))
        }
        ["compare"] if post => crate::models::compare(app, value),
        ["context"] if post => crate::context::save(&app, value),
        ["context", "preview"] if get => {
            let chat = app
                .store
                .get("chat", q.get("chatId").map(String::as_str).unwrap_or(""))?;
            let (content, images, mut provenance) = crate::context::expand(&app, &chat)?;
            provenance["content"] = json!(content);
            provenance["imageCount"] = json!(images.len());
            provenance["estimatedTokens"] = json!(content.len().div_ceil(4));
            Ok(provenance)
        }
        ["context", identity] if get => {
            crate::context::resolve(&app, &app.store.get("attachment", identity)?)
        }
        ["context", identity] if delete => {
            app.store.delete("attachment", identity)?;
            for mut chat in app.store.all("chat")? {
                if let Some(ids) = chat["contextIds"].as_array_mut() {
                    ids.retain(|v| v != identity);
                }
                app.store.put("chat", &chat)?;
            }
            Ok(json!({"ok":true}))
        }
        ["index"] if get => {
            let project = q.get("projectId").cloned().unwrap_or_default();
            let query = q.get("query").cloned().unwrap_or_default();
            tokio::task::spawn_blocking(move || crate::context::index(&app, &project, &query))
                .await?
        }
        ["tasks", chat, "fork"] if post => crate::tasks::fork(&app, chat, &value),
        ["tasks", chat, "plan"] if post => crate::tasks::plan(&app, chat, &value),
        ["tasks", chat, "enqueue"] if post => crate::tasks::enqueue(&app, chat, &value),
        ["runs", identity, action] if post && ["pause", "resume"].contains(action) => {
            crate::tasks::control(&app, identity, action)
        }
        ["runs", identity, "cancel"] if post => {
            if let Some(cancel) = app.runs.lock().get(*identity) {
                cancel.cancel();
            }
            Ok(json!({"ok":true}))
        }
        ["queue", identity] if delete => {
            let kind = if app.store.get("queued_turn", identity).is_ok() {
                "queued_turn"
            } else {
                "steering"
            };
            let mut item = app.store.get(kind, identity)?;
            if item["status"] != "queued" {
                bail!("The runtime already claimed this message");
            }
            item["status"] = json!("canceled");
            app.store.put(kind, &item)?;
            Ok(item)
        }
        ["memory"] if post => crate::tasks::save_memory(&app, &value),
        ["memory", identity] if delete => {
            app.store.delete("memory", identity)?;
            Ok(json!({"ok":true}))
        }
        ["hooks"] if post => crate::tasks::save_hook(&app, &value),
        ["hooks", identity] if delete => {
            app.store.delete("hook", identity)?;
            Ok(json!({"ok":true}))
        }
        ["schedules"] if post => crate::tasks::save_schedule(&app, &value),
        ["schedules", identity] if delete => {
            app.store.delete("schedule", identity)?;
            Ok(json!({"ok":true}))
        }
        ["schedules", identity, "run"] if post => crate::tasks::run_now(app, identity).await,
        ["notifications", identity, "read"] if post => {
            let mut notification = app.store.get("notification", identity)?;
            notification["read"] = json!(true);
            app.store.put("notification", &notification)?;
            Ok(notification)
        }
        ["teams"] if post => crate::tasks::team(app, value),
        ["worktrees"] if post => crate::worktrees::create(&app, &value),
        ["worktrees", identity, "merge"] if post => crate::worktrees::merge(&app, identity),
        ["git", "status"] if get => {
            let project = app.store.get(
                "project",
                q.get("projectId").map(String::as_str).unwrap_or(""),
            )?;
            crate::workspace::status(std::path::Path::new(text(&project, "path")))
        }
        ["git", "commit"] if post => crate::worktrees::commit(&app, &value),
        ["git", "pull-request"] if post => crate::worktrees::pull_request(&app, &value).await,
        ["review"] if get => crate::worktrees::review(
            &app,
            q.get("chatId").map(String::as_str).unwrap_or(""),
            q.get("path").map(String::as_str).unwrap_or(""),
        ),
        ["review", "decide"] if post => crate::worktrees::decide_hunk(&app, &value),
        ["review", "comment"] if post => {
            app.store.get("chat", text(&value, "chatId"))?;
            if text(&value, "content").trim().is_empty() {
                bail!("Enter a review comment");
            }
            let mut item = value;
            item["id"] = json!(id());
            item["createdAt"] = json!(now());
            app.store.put("review_comment", &item)?;
            Ok(item)
        }
        ["entity", kind, identity]
            if get
                && ["comparison", "team", "usage", "schedule", "notification"].contains(kind) =>
        {
            app.store.get(kind, identity)
        }
        _ => Err(anyhow::anyhow!(
            "Unknown Workbench feature endpoint: {path}"
        )),
    }
}
