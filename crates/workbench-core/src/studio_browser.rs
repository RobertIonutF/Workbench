use super::*;
use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, net::TcpStream, process::Command};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use tokio_util::sync::CancellationToken;

fn local_url(address: &str) -> Result<url::Url> {
    let url = url::Url::parse(address)?;
    if !["http", "https"].contains(&url.scheme())
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("Browser preview is restricted to credential-free loopback HTTP(S) URLs");
    }
    Ok(url)
}
pub(super) fn state(app: &App) -> Result<Vec<Value>> {
    let mut previews = app.store.all("studio_preview")?;
    for preview in &mut previews {
        if matches!(text(preview, "status"), "starting" | "running")
            && !app.runs.lock().contains_key(text(preview, "id"))
        {
            preview["status"] = json!("interrupted");
            preview["error"] =
                json!("Preview stopped with its Workbench process. Restart it explicitly.");
            app.store.put("studio_preview", preview)?;
        }
    }
    Ok(previews)
}
pub(super) async fn start(app: Arc<App>, value: &Value) -> Result<Value> {
    confirm(value)?;
    let project = app.store.get("project", text(value, "projectId"))?;
    let root = std::fs::canonicalize(text(&project, "path"))?;
    let address = local_url(text(value, "url"))?;
    let program = text(value, "program");
    if program.is_empty() || program.contains('\0') {
        bail!("Choose one executable and provide its arguments separately");
    }
    let args: Vec<String> = value["args"]
        .as_array()
        .context("Arguments must be a JSON array of strings")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .context("Arguments must be strings")
        })
        .collect::<Result<_>>()?;
    let mut command = Command::new(program);
    crate::runtime::command_environment(&mut command);
    command
        .current_dir(&root)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    let job = crate::runtime::CommandJob::new()?;
    let mut child = command
        .spawn()
        .context("Could not start the preview server")?;
    #[cfg(windows)]
    if let Err(error) = job.attach(&child) {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(error);
    }
    let cancel = CancellationToken::new();
    let secrets = known_secrets(&app);
    let identity = format!("preview-{}", id());
    let preview = json!({"id":identity,"projectId":project["id"],"program":program,"args":args,"url":address.as_str(),"status":"starting","logs":"","createdAt":now(),"timeoutMinutes":value["timeoutMinutes"].as_u64().unwrap_or(120).clamp(1,1440)});
    app.runs.lock().insert(identity.clone(), cancel.clone());
    app.store
        .put("studio_preview", &redact(&preview, &secrets))?;
    let result = redact(&preview, &secrets);
    tokio::spawn(async move {
        let mut record = preview;
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut out = [0; 2048];
        let mut err = [0; 2048];
        let (mut out_open, mut err_open) = (true, true);
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let deadline = tokio::time::sleep(Duration::from_secs(
            record["timeoutMinutes"].as_u64().unwrap() * 60,
        ));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _=cancel.cancelled()=>{record["status"]=json!("stopped");break;},
                _=&mut deadline=>{record["status"]=json!("expired");break;},
                status=child.wait()=>{match status{Ok(status)=>{record["exitCode"]=json!(status.code());record["status"]=json!(if status.success(){"completed"}else{"failed"});},Err(e)=>{record["status"]=json!("failed");record["error"]=json!(e.to_string());}}break;},
                chunk=stdout.read(&mut out),if out_open=>{match chunk{Ok(0)|Err(_)=>out_open=false,Ok(n)=>append_log(&mut record,"stdout",&out[..n])};},
                chunk=stderr.read(&mut err),if err_open=>{match chunk{Ok(0)|Err(_)=>err_open=false,Ok(n)=>append_log(&mut record,"stderr",&err[..n])};},
                _=tick.tick()=>{if record["status"]=="starting"&&app.client.get(address.clone()).timeout(Duration::from_secs(2)).send().await.is_ok(){record["status"]=json!("running");}},
            }
            let _ = app.store.put("studio_preview", &redact(&record, &secrets));
        }
        #[cfg(windows)]
        drop(job);
        if child.try_wait().ok().flatten().is_none() {
            if let Some(pid) = child.id() {
                crate::runtime::kill_tree(pid).await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        record["finishedAt"] = json!(now());
        let _ = app.store.put("studio_preview", &redact(&record, &secrets));
        app.runs.lock().remove(&identity);
    });
    Ok(result)
}
fn append_log(record: &mut Value, channel: &str, bytes: &[u8]) {
    let old = text(record, "logs");
    let mut log = format!("{old}[{channel}] {}", String::from_utf8_lossy(bytes));
    if log.len() > 64_000 {
        log = log
            .chars()
            .rev()
            .take(50_000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
    }
    record["logs"] = json!(log);
}
pub(super) fn stop(app: &App, value: &Value) -> Result<Value> {
    let record = app.store.get("studio_preview", text(value, "id"))?;
    if let Some(cancel) = app.runs.lock().get(text(&record, "id")) {
        cancel.cancel();
    }
    Ok(json!({"ok":true}))
}
fn browser_path(value: &Value) -> Result<PathBuf> {
    if !text(value, "browserPath").is_empty() {
        return Ok(std::fs::canonicalize(text(value, "browserPath"))?);
    }
    let mut paths = vec![];
    for directory in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(directory) {
            for relative in [
                "Microsoft/Edge/Application/msedge.exe",
                "Google/Chrome/Application/chrome.exe",
                "Chromium/Application/chrome.exe",
            ] {
                paths.push(PathBuf::from(&base).join(relative));
            }
        }
    }
    if let Some(path) = paths.into_iter().find(|p| p.is_file()) {
        return Ok(path);
    }
    for name in [
        "google-chrome",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
    ] {
        if let Some(paths) = std::env::var_os("PATH") {
            for parent in std::env::split_paths(&paths) {
                let path = parent.join(name);
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }
    bail!("Install Chromium, Chrome or Edge, or choose its executable path")
}
struct Cdp {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    sequence: u64,
    console: Vec<Value>,
    errors: Vec<Value>,
}
impl Cdp {
    fn event(&mut self, event: Value) {
        match text(&event, "method") {
            "Runtime.consoleAPICalled" => {
                let entry = json!({"type":event["params"]["type"],"text":event["params"]["args"].as_array().map(|a|a.iter().map(|v|v.get("value").unwrap_or(&v["description"]).to_string()).collect::<Vec<_>>().join(" ")).unwrap_or_default()});
                if event["params"]["type"] == "error" {
                    self.errors.push(entry.clone());
                }
                self.console.push(entry);
            }
            "Runtime.exceptionThrown" => self
                .errors
                .push(json!({"type":"exception","details":event["params"]["exceptionDetails"]})),
            "Network.loadingFailed" => {
                if event["params"]["canceled"] != true {
                    self.errors
                        .push(json!({"type":"network","error":event["params"]["errorText"]}));
                }
            }
            "Network.responseReceived"
                if event["params"]["response"]["status"]
                    .as_u64()
                    .is_some_and(|s| s >= 400) =>
            {
                self.errors.push(json!({"type":"http","status":event["params"]["response"]["status"],"url":event["params"]["response"]["url"]}));
            }
            _ => {}
        }
        if self.console.len() > 200 {
            self.console.remove(0);
        }
        if self.errors.len() > 200 {
            self.errors.remove(0);
        }
    }
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let expected = self.sequence;
        self.socket
            .send(Message::Text(
                json!({"id":expected,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let message = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .context("Browser command timed out")?
                .context("Browser debugging connection closed")??;
            if let Message::Text(message) = message {
                let value: Value = serde_json::from_str(&message)?;
                if value["id"] == expected {
                    if value.get("error").is_some() {
                        bail!("Browser command failed: {}", value["error"]);
                    }
                    return Ok(value["result"].clone());
                }
                self.event(value);
            }
        }
    }
    async fn evaluate(&mut self, expression: String) -> Result<Value> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({"expression":expression,"returnByValue":true,"awaitPromise":true}),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            bail!("Browser script failed: {}", result["exceptionDetails"]);
        }
        Ok(result["result"]["value"].clone())
    }
}
pub(super) async fn test(app: Arc<App>, value: &Value) -> Result<Value> {
    confirm(value)?;
    let address = local_url(text(value, "url"))?;
    let executable = browser_path(value)?;
    let identity = id();
    let profile = app.data_dir.join("browser-profiles").join(&identity);
    std::fs::create_dir_all(&profile)?;
    let mut command = Command::new(executable);
    crate::runtime::command_environment(&mut command);
    command
        .args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--remote-debugging-port=0",
            "--window-size=1440,960",
        ])
        .arg(format!("--user-data-dir={}", profile.to_string_lossy()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    let job = crate::runtime::CommandJob::new()?;
    let mut child = command
        .spawn()
        .context("Headless browser could not start")?;
    #[cfg(windows)]
    if let Err(e) = job.attach(&child) {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(e);
    }
    let started = std::time::Instant::now();
    let outcome=tokio::time::timeout(Duration::from_secs(90),async{
        let mut port=None;
        for _ in 0..100{if let Ok(contents)=std::fs::read_to_string(profile.join("DevToolsActivePort")){port=contents.lines().next().and_then(|p|p.parse::<u16>().ok());if port.is_some(){break;}}
        if child.try_wait()?.is_some(){bail!("Headless browser exited before its debugging port became available");}tokio::time::sleep(Duration::from_millis(100)).await;}
        let port=port.context("Headless browser did not expose its loopback debugging port")?;
        let page:Value=app.client.put(format!("http://127.0.0.1:{port}/json/new?about:blank")).send().await?.json().await?;
        let websocket=text(&page,"webSocketDebuggerUrl");let parsed=url::Url::parse(websocket)?;if parsed.scheme()!="ws"||parsed.host_str()!=Some("127.0.0.1"){bail!("Browser returned an invalid local debugging endpoint");}
        let(socket,_)=tokio_tungstenite::connect_async(websocket).await?;let mut cdp=Cdp{socket,sequence:0,console:vec![],errors:vec![]};
        cdp.call("Page.enable",json!({})).await?;cdp.call("Runtime.enable",json!({})).await?;cdp.call("Network.enable",json!({})).await?;
        let navigation=cdp.call("Page.navigate",json!({"url":address.as_str()})).await?;if navigation.get("errorText").is_some(){bail!("Page navigation failed: {}",navigation["errorText"]);}
        for _ in 0..100{if cdp.evaluate("document.readyState === 'complete'".into()).await?==true{break;}tokio::time::sleep(Duration::from_millis(100)).await;}
        tokio::time::sleep(Duration::from_millis(value["waitMs"].as_u64().unwrap_or(1200).min(10_000))).await;
        let navigated=cdp.evaluate("location.href".into()).await?;
        local_url(navigated.as_str().context("Page URL is unavailable")?)?;
        let mut steps=vec![];
        for step in value["steps"].as_array().unwrap_or(&vec![]).iter().take(40){
            let navigated=cdp.evaluate("location.href".into()).await?;
            local_url(navigated.as_str().context("Page URL is unavailable")?)?;
            let selector=serde_json::to_string(text(step,"selector"))?;let content=serde_json::to_string(text(step,"value"))?;
            let expression=match text(step,"type"){
                "click"=>format!("(()=>{{const e=document.querySelector({selector});if(!e)throw Error('Element not found');e.click();return true;}})()"),
                "fill"=>format!("(()=>{{const e=document.querySelector({selector});if(!e)throw Error('Element not found');const p=e instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e,{content});e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));return true;}})()"),
                _=>bail!("Browser steps support click and fill operations")
            };
            match cdp.evaluate(expression).await{Ok(result)=>steps.push(json!({"type":step["type"],"selector":step["selector"],"ok":result==true})),Err(e)=>{steps.push(json!({"type":step["type"],"selector":step["selector"],"ok":false,"error":e.to_string()}));break;}}
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let mut assertions=vec![];
        for assertion in value["assertions"].as_array().unwrap_or(&vec![]).iter().take(50){
            let selector=serde_json::to_string(assertion["selector"].as_str().unwrap_or("body"))?;let contains=serde_json::to_string(text(assertion,"contains"))?;
            let expression=format!("(()=>{{const e=document.querySelector({selector});return {{exists:!!e,text:e?.textContent?.slice(0,1000)||'',passed:!!e&&e.textContent.includes({contains})}};}})()");
            match cdp.evaluate(expression).await{Ok(result)=>assertions.push(json!({"selector":assertion["selector"],"contains":assertion["contains"],"passed":result["passed"],"actual":result["text"]})),Err(e)=>assertions.push(json!({"selector":assertion["selector"],"passed":false,"error":e.to_string()}))}
        }
        let page=cdp.evaluate("({title:document.title,url:location.href,text:document.body?.innerText?.slice(0,6000)||''})".into()).await?;
        let image=cdp.call("Page.captureScreenshot",json!({"format":"png","captureBeyondViewport":false})).await?;
        let folder=app.data_dir.join("browser-artifacts");std::fs::create_dir_all(&folder)?;let screenshot=folder.join(format!("{identity}.png"));std::fs::write(&screenshot,STANDARD.decode(text(&image,"data"))?)?;
        let passed=steps.iter().all(|s|s["ok"]==true)&&assertions.iter().all(|a|a["passed"]==true)&&(value["failOnErrors"]==false||cdp.errors.is_empty());
        Ok::<_,anyhow::Error>(json!({"id":identity,"url":address.as_str(),"passed":passed,"page":page,"steps":steps,"assertions":assertions,"console":cdp.console,"errors":cdp.errors,"screenshot":screenshot,"createdAt":now(),"elapsedMs":started.elapsed().as_millis() as u64}))
    }).await;
    #[cfg(windows)]
    drop(job);
    if child.try_wait().ok().flatten().is_none() {
        if let Some(pid) = child.id() {
            crate::runtime::kill_tree(pid).await;
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let result = match outcome {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            json!({"id":identity,"url":address.as_str(),"passed":false,"error":e.to_string(),"createdAt":now()})
        }
        Err(_) => {
            json!({"id":identity,"url":address.as_str(),"passed":false,"error":"Browser test timed out and its process was stopped","createdAt":now()})
        }
    };
    let result = redact(&result, &known_secrets(&app));
    app.store.put("studio_browser_test", &result)?;
    Ok(result)
}
pub(super) fn artifact(app: &App, value: &Value) -> Result<Value> {
    let entry = app.store.get("studio_browser_test", text(value, "id"))?;
    let path = std::fs::canonicalize(text(&entry, "screenshot"))?;
    if !path.starts_with(std::fs::canonicalize(
        app.data_dir.join("browser-artifacts"),
    )?) {
        bail!("Artifact escapes its managed directory");
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() > 8 * 1024 * 1024 {
        bail!("Screenshot exceeds preview limit");
    }
    Ok(json!({"imageDataUrl":format!("data:image/png;base64,{}",STANDARD.encode(bytes))}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_only_targets_loopback() {
        assert!(local_url("http://127.0.0.1:3000/test").is_ok());
        for bad in [
            "file:///C:/secret",
            "http://example.com",
            "https://user:secret@localhost/",
            "javascript:alert(1)",
        ] {
            assert!(local_url(bad).is_err());
        }
    }
}
