use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{io::Write, path::Path, time::Duration};

pub async fn request(
    info: &Value,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let url = format!(
        "{}/api/{path}",
        crate::app::text(info, "baseUrl").trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut request = client
        .request(method, url)
        .header("X-Workbench-Token", crate::app::text(info, "token"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .context("Workbench runtime is unavailable; start the desktop app or a local server")?;
    let status = response.status();
    let value: Value = response.json().await?;
    if !status.is_success() {
        bail!(
            "{}",
            value["error"]
                .as_str()
                .unwrap_or("Workbench request failed")
        );
    }
    Ok(value)
}
pub async fn watch(info: &Value, path: &Path) -> Result<()> {
    let source = std::fs::canonicalize(path)?;
    let state = request(info, reqwest::Method::GET, "state", None).await?;
    let installed = state["plugins"]
        .as_array()
        .and_then(|a| {
            a.iter().find(|p| {
                std::fs::canonicalize(crate::app::text(p, "source")).ok() == Some(source.clone())
            })
        })
        .cloned();
    let mut plugin = if let Some(plugin) = installed {
        plugin
    } else {
        request(
            info,
            reqwest::Method::POST,
            "plugins/install",
            Some(json!({"path":source})),
        )
        .await?
    };
    println!(
        "Watching {}. Skill-only changes reload between turns; changed MCP configuration requires review. Press Ctrl+C to stop.",
        source.display()
    );
    loop {
        tokio::select! {_=tokio::signal::ctrl_c()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
        match crate::plugins::inspect(&source, crate::app::text(&plugin, "id")) {
            Ok(candidate) if candidate["digest"] != plugin["digest"] => {
                let state = request(info, reqwest::Method::GET, "state", None).await?;
                if state["runs"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|r| r["status"] == "running"))
                {
                    continue;
                }
                let unchanged_servers = candidate["servers"] == plugin["servers"];
                let enabled = plugin["enabled"] == true;
                plugin = request(
                    info,
                    reqwest::Method::POST,
                    &format!("plugins/{}/update", crate::app::text(&plugin, "id")),
                    Some(json!({"path":source})),
                )
                .await?;
                if unchanged_servers && enabled {
                    plugin = request(
                        info,
                        reqwest::Method::POST,
                        &format!("plugins/{}/enable", crate::app::text(&plugin, "id")),
                        Some(json!({"enabled":true})),
                    )
                    .await?;
                }
                println!(
                    "Reloaded {} {}{}",
                    crate::app::text(&plugin, "name"),
                    crate::app::text(&plugin, "version"),
                    if unchanged_servers {
                        ""
                    } else {
                        "; enable after reviewing changed servers"
                    }
                );
            }
            Err(error) => eprintln!("Authoring diagnostic: {error}"),
            _ => {}
        }
    }
    Ok(())
}
pub async fn chat(
    info: &Value,
    prompt: &str,
    project: Option<&Path>,
    connection_id: Option<&str>,
    model: Option<&str>,
    mode: &str,
) -> Result<()> {
    let state = request(info, reqwest::Method::GET, "state", None).await?;
    let connections = state["connections"].as_array().context("No connections")?;
    let connection = if let Some(id) = connection_id {
        connections.iter().find(|c| c["id"] == id)
    } else {
        connections.first()
    }
    .context("Configure a model connection in Workbench first")?;
    let project = if let Some(path) = project {
        let path = std::fs::canonicalize(path)?;
        Some(
            request(
                info,
                reqwest::Method::POST,
                "projects",
                Some(json!({"path":path})),
            )
            .await?,
        )
    } else {
        None
    };
    let chat=request(info,reqwest::Method::POST,"chats",Some(json!({"connectionId":connection["id"],"modelId":model.unwrap_or(crate::app::text(connection,"modelId")),"projectId":project.map(|p|p["id"].clone()),"executionMode":mode}))).await?;
    let chat_id = crate::app::text(&chat, "id");
    let run = request(
        info,
        reqwest::Method::POST,
        &format!("chats/{chat_id}/turns"),
        Some(json!({"text":prompt})),
    )
    .await?;
    let run_id = crate::app::text(&run, "id");
    let mut cursor = 0;
    let mut handled = std::collections::HashSet::new();
    loop {
        let events = request(
            info,
            reqwest::Method::GET,
            &format!("events?after={cursor}&chatId={chat_id}"),
            None,
        )
        .await?;
        cursor = events["cursor"].as_i64().unwrap_or(cursor);
        if let Some(events) = events["events"].as_array() {
            for event in events {
                if event["kind"] == "text_delta" {
                    print!("{}", crate::app::text(&event["data"], "text"));
                    std::io::stdout().flush()?;
                } else if event["kind"] == "tool_start" {
                    eprintln!("\nTool: {}", crate::app::text(&event["data"], "name"));
                }
            }
        }
        let state = request(info, reqwest::Method::GET, "state", None).await?;
        if let Some(approvals) = state["approvals"].as_array() {
            for approval in approvals
                .iter()
                .filter(|a| a["runId"] == run_id && a["status"] == "pending")
            {
                let approval_id = crate::app::text(approval, "id");
                if !handled.insert(approval_id.to_owned()) {
                    continue;
                }
                eprintln!(
                    "\n{}\n{}\nAllow once? [y/N]",
                    crate::app::text(approval, "title"),
                    serde_json::to_string_pretty(&approval["details"])?
                );
                let (tx, rx) = tokio::sync::oneshot::channel();
                std::thread::spawn(move || {
                    let mut line = String::new();
                    let _ = std::io::stdin().read_line(&mut line);
                    let _ = tx.send(line.trim().eq_ignore_ascii_case("y"));
                });
                let accepted = tokio::select! {answer=rx=>answer.unwrap_or(false),_=tokio::signal::ctrl_c()=>{let _=request(info,reqwest::Method::POST,&format!("runs/{run_id}/cancel"),Some(json!({}))).await;return Ok(())}};
                request(
                    info,
                    reqwest::Method::POST,
                    &format!("approvals/{approval_id}"),
                    Some(json!({"decision":if accepted{"allow_once"}else{"deny"}})),
                )
                .await?;
            }
        }
        if let Some(run) = state["runs"]
            .as_array()
            .and_then(|a| a.iter().find(|r| r["id"] == run_id))
            && run["status"] != "running"
        {
            // The model can finish between the event poll and this state read.
            // Drain its final durable events before returning to the terminal.
            loop {
                let remaining = request(
                    info,
                    reqwest::Method::GET,
                    &format!("events?after={cursor}&chatId={chat_id}"),
                    None,
                )
                .await?;
                let next = remaining["cursor"].as_i64().unwrap_or(cursor);
                if next == cursor {
                    break;
                }
                if let Some(events) = remaining["events"].as_array() {
                    for event in events {
                        if event["kind"] == "text_delta" {
                            print!("{}", crate::app::text(&event["data"], "text"));
                        }
                    }
                }
                cursor = next;
            }
            println!();
            if run["status"] == "failed" {
                bail!("{}", crate::app::text(run, "error"));
            }
            return Ok(());
        }
        tokio::select! {_=tokio::signal::ctrl_c()=>{let _=request(info,reqwest::Method::POST,&format!("runs/{run_id}/cancel"),Some(json!({}))).await;tokio::time::sleep(Duration::from_millis(500)).await;return Ok(())},_=tokio::time::sleep(Duration::from_millis(150))=>{}}
    }
}
