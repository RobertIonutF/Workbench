use anyhow::{Context, Result};
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use workbench_core::App;

async fn call(app: Arc<App>, path: &str, value: Value) -> Result<Value> {
    workbench_core::studio::dispatch(app, &axum::http::Method::POST, path, &HashMap::new(), value)
        .await
        .context("Studio route not found")?
}
const SERVER: &str = r#"import sys,json
print('fixture-secret-for-redaction',file=sys.stderr,flush=True)
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    p=m.get('params',{}); method=m['method']
    if method=='initialize': r={'protocolVersion':'2025-11-25','capabilities':{'tools':{},'resources':{},'prompts':{}},'serverInfo':{'name':'studio-fixture','version':'1'}}
    elif method=='tools/list': r={'tools':[{'name':'calculate','description':'Add actual numbers','inputSchema':{'type':'object','properties':{'numbers':{'type':'array','items':{'type':'number'}}},'required':['numbers'],'additionalProperties':False}}]}
    elif method=='tools/call': r={'content':[{'type':'text','text':json.dumps({'sum':sum(p['arguments']['numbers'])})}]}
    elif method=='resources/list': r={'resources':[{'uri':'fixture://guide','name':'Fixture guide'}]}
    elif method=='resources/templates/list': r={'resourceTemplates':[]}
    elif method=='resources/read': r={'contents':[{'uri':p['uri'],'mimeType':'text/plain','text':'Actual fixture guide'}]}
    elif method=='prompts/list': r={'prompts':[{'name':'greet','arguments':[{'name':'person','required':True}]}]}
    elif method=='prompts/get': r={'messages':[{'role':'user','content':{'type':'text','text':'Hello '+p['arguments']['person']}}]}
    else:
        print(json.dumps({'jsonrpc':'2.0','id':m['id'],'error':{'code':-32601,'message':'Unknown method'}}),flush=True);continue
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':r}),flush=True)
"#;
async fn fixture() -> Result<(tempfile::TempDir, Arc<App>, Value)> {
    let directory = tempfile::tempdir()?;
    let app = App::new(Some(directory.path().join("data"))).await?;
    let source = directory.path().join("plugin");
    workbench_core::plugins::create(
        &json!({"name":"studio-fixture","path":source,"template":"combined","language":"python","description":"Actual studio fixture"}),
    )?;
    std::fs::write(source.join("server/server.py"), SERVER)?;
    let plugin = workbench_core::plugins::install(&app, &source, None)?;
    workbench_core::auth::set_secret(&app, "fixture-key", "fixture-secret-for-redaction")?;
    Ok((directory, app, plugin))
}
#[tokio::test]
async fn studio_exercises_tools_resources_prompts_and_asserts_real_results() -> Result<()> {
    let (_directory, app, plugin) = fixture().await?;
    let mut request = json!({"pluginId":plugin["id"],"serverName":"calculator","method":"discover","confirmed":true});
    let discovery = call(app.clone(), "studio/invoke", request.clone()).await?;
    assert_eq!(discovery["result"]["tools"][0]["name"], "calculate");
    assert_eq!(
        discovery["result"]["resources"][0]["uri"],
        "fixture://guide"
    );
    assert_eq!(discovery["result"]["prompts"][0]["name"], "greet");
    assert!(
        !discovery
            .to_string()
            .contains("fixture-secret-for-redaction")
    );
    assert!(discovery["logs"].to_string().contains("redacted"));
    request["method"] = json!("tools/call");
    request["params"] = json!({"name":"calculate","arguments":{"numbers":[1.25,2.75]}});
    let actual = call(app.clone(), "studio/invoke", request.clone()).await?;
    let content: Value =
        serde_json::from_str(actual["result"]["content"][0]["text"].as_str().unwrap())?;
    assert_eq!(content["sum"], 4.0);
    let case=call(app.clone(),"studio/case",json!({"name":"Actual fractional sum","pluginId":plugin["id"],"serverName":"calculator","method":"tools/call","params":request["params"],"expect":{"pointer":"/result/content/0/text","equals":actual["result"]["content"][0]["text"]}})).await?;
    assert_eq!(
        call(
            app.clone(),
            "studio/cases/run",
            json!({"id":case["id"],"confirmed":true})
        )
        .await?["passed"],
        true
    );
    request["params"] = json!({"name":"calculate","arguments":{"numbers":"bad"}});
    let invalid = call(app.clone(), "studio/invoke", request.clone()).await?;
    assert_eq!(invalid["ok"], false);
    assert!(invalid["error"].as_str().unwrap().contains("validation"));
    request["method"] = json!("resources/read");
    request["params"] = json!({"uri":"fixture://guide"});
    assert_eq!(
        call(app.clone(), "studio/invoke", request.clone()).await?["result"]["contents"][0]["text"],
        "Actual fixture guide"
    );
    request["method"] = json!("prompts/get");
    request["params"] = json!({"name":"greet","arguments":{"person":"Ada"}});
    assert_eq!(
        call(app.clone(), "studio/invoke", request.clone()).await?["result"]["messages"][0]["content"]
            ["text"],
        "Hello Ada"
    );
    request["confirmed"] = json!(false);
    assert!(call(app.clone(), "studio/invoke", request).await.is_err());
    Ok(())
}
#[tokio::test]
async fn portable_backup_restores_credentials_and_rebases_plugin_files() -> Result<()> {
    let (directory, app, plugin) = fixture().await?;
    let source_secret = workbench_core::auth::get_secret(&app, "fixture-key")?;
    let backup=call(app.clone(),"studio/backup/create",json!({"confirmed":true,"passphrase":"fixture backup passphrase","path":directory.path().join("profile.wbb")})).await?;
    let bytes = std::fs::read(backup["path"].as_str().unwrap())?;
    assert!(!String::from_utf8_lossy(&bytes).contains(&source_secret));
    let destination = directory.path().join("restored");
    let restored=call(app.clone(),"studio/backup/restore",json!({"confirmed":true,"passphrase":"fixture backup passphrase","path":backup["path"],"destination":destination})).await?;
    assert_eq!(restored["restartRequired"], false);
    let restored_app = App::new(Some(destination.clone())).await?;
    assert_eq!(
        workbench_core::auth::get_secret(&restored_app, "fixture-key")?,
        source_secret
    );
    let restored_plugin = restored_app
        .store
        .get("plugin", plugin["id"].as_str().unwrap())?;
    assert!(
        std::path::Path::new(restored_plugin["path"].as_str().unwrap()).starts_with(&destination)
    );
    assert!(
        std::path::Path::new(restored_plugin["path"].as_str().unwrap())
            .join("server/server.py")
            .exists()
    );
    assert!(call(app.clone(),"studio/backup/restore",json!({"confirmed":true,"passphrase":"wrong backup passphrase","path":backup["path"],"destination":directory.path().join("wrong")})).await.is_err());
    assert!(!directory.path().join("wrong").exists());
    Ok(())
}
#[tokio::test]
async fn pending_restore_waits_for_restart_and_keeps_recovery_copy() -> Result<()> {
    let (directory, app, _) = fixture().await?;
    app.store
        .put("memory", &json!({"id":"fixture","text":"Before backup"}))?;
    let backup=call(app.clone(),"studio/backup/create",json!({"confirmed":true,"passphrase":"fixture backup passphrase","path":directory.path().join("profile.wbb")})).await?;
    app.store
        .put("memory", &json!({"id":"fixture","text":"After backup"}))?;
    let staged = call(
        app.clone(),
        "studio/backup/restore",
        json!({"confirmed":true,"passphrase":"fixture backup passphrase","path":backup["path"]}),
    )
    .await?;
    assert_eq!(staged["restartRequired"], true);
    assert_eq!(app.store.get("memory", "fixture")?["text"], "After backup");
    drop(app);
    let restored = App::new(Some(directory.path().join("data"))).await?;
    assert_eq!(
        restored.store.get("memory", "fixture")?["text"],
        "Before backup"
    );
    let info: Value =
        serde_json::from_slice(&std::fs::read(restored.data_dir.join("last-restore.json"))?)?;
    assert!(
        std::path::Path::new(info["recovery"].as_str().unwrap())
            .join("workbench.sqlite")
            .exists()
    );
    Ok(())
}
#[tokio::test]
async fn preview_server_starts_serves_and_stops_with_its_owned_process() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let project_dir = directory.path().join("project");
    std::fs::create_dir(&project_dir)?;
    std::fs::write(project_dir.join("index.html"), "preview-fixture")?;
    std::fs::write(
        project_dir.join("server.py"),
        "import http.server,sys\nprint('fixture-preview-secret',file=sys.stderr,flush=True)\nhttp.server.test(HandlerClass=http.server.SimpleHTTPRequestHandler,ServerClass=http.server.ThreadingHTTPServer,port=int(sys.argv[1]),bind='127.0.0.1')\n",
    )?;
    let app = App::new(Some(directory.path().join("data"))).await?;
    workbench_core::auth::set_secret(&app, "preview-key", "fixture-preview-secret")?;
    let project = app.project(json!({"path":project_dir}))?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let url = format!("http://127.0.0.1:{port}");
    let preview=call(app.clone(),"studio/browser/start",json!({"projectId":project["id"],"program":"python","args":["-u","server.py",port.to_string()],"url":url,"confirmed":true})).await?;
    let mut served = false;
    for _ in 0..60 {
        if let Ok(response) = app
            .client
            .get(&url)
            .timeout(Duration::from_secs(1))
            .send()
            .await
        {
            assert!(response.text().await?.contains("preview-fixture"));
            served = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(served);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let saved = app
        .store
        .get("studio_preview", preview["id"].as_str().unwrap())?;
    assert!(saved["logs"].as_str().unwrap().contains("[redacted]"));
    assert!(!saved.to_string().contains("fixture-preview-secret"));
    call(
        app.clone(),
        "studio/browser/stop",
        json!({"id":preview["id"]}),
    )
    .await?;
    for _ in 0..60 {
        if !app
            .runs
            .lock()
            .contains_key(preview["id"].as_str().unwrap())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !app.runs
            .lock()
            .contains_key(preview["id"].as_str().unwrap())
    );
    assert!(
        app.client
            .get(url)
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .is_err()
    );
    Ok(())
}
#[tokio::test]
#[cfg(windows)]
async fn actual_headless_browser_fills_clicks_asserts_and_captures() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let app = App::new(Some(directory.path().join("data"))).await?;
    workbench_core::auth::set_secret(&app, "browser-key", "fixture-browser-secret")?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(listener,Router::new().route("/",get(||async{axum::response::Html("<!doctype html><html><head><title>Studio fixture</title><link rel='icon' href='data:,'></head><body><input id='name'><button id='go' onclick=\"document.querySelector('#result').textContent='Hello '+document.querySelector('#name').value\">Go</button><p id='result'>Waiting</p><script>console.log('fixture loaded', 'fixture-browser-secret')</script></body></html>")}))).await.unwrap();
    });
    let value = json!({"confirmed":true,"url":format!("http://{address}/"),"waitMs":50,"steps":[{"type":"fill","selector":"#name","value":"Ada"},{"type":"click","selector":"#go"}],"assertions":[{"selector":"#result","contains":"Hello Ada"}],"failOnErrors":true});
    let result = call(app.clone(), "studio/browser/test", value).await?;
    assert_eq!(result["passed"], true, "{result}");
    assert_eq!(result["page"]["title"], "Studio fixture");
    assert!(!result.to_string().contains("fixture-browser-secret"));
    assert!(
        !app.store
            .get("studio_browser_test", result["id"].as_str().unwrap())?
            .to_string()
            .contains("fixture-browser-secret")
    );
    assert!(
        result["console"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.to_string().contains("fixture loaded"))
    );
    let image = call(
        app.clone(),
        "studio/browser/artifact",
        json!({"id":result["id"]}),
    )
    .await?;
    assert!(
        image["imageDataUrl"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,iVBOR")
    );
    Ok(())
}
#[tokio::test]
async fn plugin_generation_uses_a_real_provider_response_and_writes_reviewable_source() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let app = App::new(Some(directory.path().join("data"))).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(listener,Router::new().route("/v1/chat/completions",post(|Json(body):Json<Value>|async move{assert_eq!(body["model"],"fixture-author");let text=json!({"skillBody":"Discover actual tools and report their results.","serverSource":SERVER,"testSuggestions":["Add fractional numbers"]}).to_string();let stream=format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":{"content":text},"finish_reason":null}]}),json!({"choices":[{"delta":{},"finish_reason":"stop"}]}));([("content-type","text/event-stream")],stream)}))).await.unwrap();
    });
    let connection=app.connection(json!({"label":"Fixture author","preset":"custom","protocol":"openai_chat","baseUrl":format!("http://{address}/v1"),"authKind":"none","modelId":"fixture-author","contextWindow":8192,"tools":false}))?;
    let output = directory.path().join("generated");
    let result=call(app.clone(),"studio/generate",json!({"confirmed":true,"name":"generated-fixture","path":output,"description":"Create actual fixture operations","language":"python","connectionId":connection["id"]})).await?;
    assert_eq!(result["draft"], true);
    assert_eq!(
        std::fs::read_to_string(output.join("server/server.py"))?,
        SERVER
    );
    assert!(
        output
            .join("skills/generated-fixture-workflow/SKILL.md")
            .exists()
    );
    let installed = workbench_core::plugins::install(&app, &output, None)?;
    let discovery=call(app.clone(),"studio/invoke",json!({"pluginId":installed["id"],"serverName":"calculator","method":"discover","confirmed":true})).await?;
    assert_eq!(discovery["result"]["tools"][0]["name"], "calculate");
    Ok(())
}

#[tokio::test]
async fn updater_rechecks_artifact_before_waiting_or_executing() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let artifact = directory.path().join("fixture.exe");
    std::fs::write(&artifact, b"This fixture must never execute")?;
    let failure = workbench_core::studio::apply_update_after_exit(0, &artifact, &"0".repeat(64))
        .await
        .unwrap_err();
    assert!(failure.to_string().contains("digest changed"));
    assert!(
        workbench_core::studio::apply_update_after_exit(
            std::process::id(),
            &artifact,
            &"0".repeat(64)
        )
        .await
        .is_err()
    );
    Ok(())
}
