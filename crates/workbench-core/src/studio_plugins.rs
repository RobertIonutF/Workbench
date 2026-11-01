use super::*;
use anyhow::Context;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Instant};
use tokio_util::sync::CancellationToken;

pub(super) async fn invoke(app: Arc<App>, value: &Value) -> Result<Value> {
    confirm(value)?;
    let plugin = app.store.get("plugin", text(value, "pluginId"))?;
    if plugin["enabled"] != true {
        bail!("Enable this plugin before invoking its tools");
    }
    let server = plugin["servers"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["name"] == value["serverName"]))
        .context("Choose an installed MCP server")?;
    let config = crate::plugins::runtime_server(&app, &plugin, server).await?;
    let current = app.store.get("plugin", text(&plugin, "id"))?;
    if current["enabled"] != true
        || current["digest"] != plugin["digest"]
        || current["path"] != plugin["path"]
    {
        bail!("Plugin changed or was disabled while authentication completed");
    }
    let mut secrets = known_secrets(&app);
    if let Some(headers) = config["_workbenchHeaders"].as_object() {
        for v in headers.values().filter_map(Value::as_str) {
            secrets.push(v.into());
            if let Some(token) = v.strip_prefix("Bearer ") {
                secrets.push(token.into());
            }
        }
    }
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let data = app
        .data_dir
        .join("plugin-data")
        .join(text(&plugin, "id"))
        .join(text(server, "name"))
        .join("studio");
    let mut session = crate::mcp::Session::connect(
        &config,
        Path::new(text(&plugin, "path")),
        &data,
        app.client.clone(),
        &cancel,
    )
    .await?;
    let method = if text(value, "method").is_empty() {
        "discover"
    } else {
        text(value, "method")
    };
    let params = value.get("params").cloned().unwrap_or(json!({}));
    let operation=async {
        if method=="discover" {
            let capabilities=session.capabilities().clone();
            let tools=if capabilities.get("tools").is_some(){session.tools(&cancel).await?}else{vec![]};
            let mut result=json!({"tools":tools,"capabilities":capabilities,"resources":[],"resourceTemplates":[],"prompts":[]});
            for (cap, rpc, field) in [("resources","resources/list","resources"),("resources","resources/templates/list","resourceTemplates"),("prompts","prompts/list","prompts")] {
                if capabilities.get(cap).is_some() {
                    match session.call(rpc,json!({}),&cancel).await {Ok(v)=>result[field]=v[field].clone(),Err(e)=>result[format!("{field}Diagnostic")]=json!(e.to_string())}
                }
            }
            return Ok(result);
        }
        if !["tools/call","resources/read","prompts/get","resources/list","prompts/list"].contains(&method) {bail!("Studio permits discovery, tool invocation, resource reading and prompt rendering only");}
        if method=="tools/call" {
            let tools=session.tools(&cancel).await?;
            let tool=tools.iter().find(|t|t["name"]==params["name"]).context("Tool was not advertised by this server")?;
            let validator=jsonschema::validator_for(&tool["inputSchema"]).map_err(|e|anyhow::anyhow!(e.to_string()))?;
            validator.validate(&params["arguments"]).map_err(|e|anyhow::anyhow!("Tool arguments failed validation: {e}"))?;
        }
        let current=app.store.get("plugin",text(&plugin,"id"))?;
        if current["enabled"]!=true || current["digest"]!=plugin["digest"] {bail!("The plugin was changed or disabled; discover it again");}
        session.call(method,params.clone(),&cancel).await
    }.await;
    session.close().await;
    let logs = session.drain_logs();
    let public = match operation {
        Ok(result) => json!({"ok":result["isError"]!=true,"result":result}),
        Err(error) => json!({"ok":false,"error":error.to_string()}),
    };
    let mut public = redact(&public, &secrets);
    public["logs"] = redact(&json!(logs), &secrets);
    public["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
    trace(
        &app,
        redact(
            &json!({"pluginId":plugin["id"],"serverName":server["name"],"method":method,"params":params,"response":public,"elapsedMs":public["elapsedMs"]}),
            &secrets,
        ),
    )?;
    Ok(public)
}
pub(super) async fn generate(app: Arc<App>, value: &Value) -> Result<Value> {
    confirm(value)?;
    let name = text(value, "name");
    let path = Path::new(text(value, "path"));
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || !name.chars().next().unwrap().is_ascii_alphanumeric()
    {
        bail!("Choose a lowercase hyphenated package name");
    }
    if text(value, "path").is_empty()
        || (path.exists() && (!path.is_dir() || std::fs::read_dir(path)?.next().is_some()))
    {
        bail!("Choose a new empty source directory before generating");
    }
    let description = text(value, "description");
    if description.trim().is_empty() {
        bail!("Describe the plugin workflow and actual operations it should perform");
    }
    let connection = app.store.get("connection", text(value, "connectionId"))?;
    let model = if text(value, "modelId").is_empty() {
        text(&connection, "modelId")
    } else {
        text(value, "modelId")
    };
    if model.is_empty() {
        bail!("Choose the exact model for generation");
    }
    let language = if ["rust", "typescript", "python"].contains(&text(value, "language")) {
        text(value, "language")
    } else {
        "python"
    };
    let contract = format!(
        "Author an independently usable skills/MCP plugin. Return ONLY a JSON object with string fields skillBody and serverSource, and an array of strings testSuggestions. The server language is {language}. Implement actual JSON-RPC 2.0 newline-delimited stdio MCP initialize/notifications/initialized, tools/list and tools/call. MCP protocolVersion2025-11-25, advertise accurate inputSchema. Validate arguments, return protocol errors and isError tool failures. Never log secrets or pretend external actions succeeded. Python must use only standard library; TypeScript response must be runnable JavaScript .mjs using Node built-ins; Rust must use only std and serde_json. Do not embed credentials, authentication tokens or install commands. The skill should discover actual tool names and describe evidence and user approvals. Implement the requested domain behavior and explicitly report unavailable external configuration. Do not substitute a calculator. Source is a draft that requires review and real tests."
    );
    let generation = crate::providers::generate(
        &app,
        &connection,
        model,
        &contract,
        &[crate::providers::Message::new("user", description.into())],
        &[],
        &CancellationToken::new(),
        |_| {},
    )
    .await?;
    let output = generation.text.trim();
    let output = output
        .strip_prefix("```json")
        .or_else(|| output.strip_prefix("```"))
        .unwrap_or(output)
        .trim();
    let output = output.strip_suffix("```").unwrap_or(output).trim();
    let authored:Value=serde_json::from_str(output).context("The selected model did not return the requested plugin JSON. No source files were written; retry with a model supporting structured output")?;
    let skill = text(&authored, "skillBody");
    let source = text(&authored, "serverSource");
    if skill.trim().is_empty()
        || source.trim().is_empty()
        || skill.len() > 128 * 1024
        || source.len() > 512 * 1024
    {
        bail!("Generated plugin omitted valid bounded skill/server source");
    }
    let mut options = value.clone();
    options["template"] = json!("combined");
    options["language"] = json!(language);
    options["skillBody"] = json!(skill);
    let result = crate::plugins::create(&options)?;
    let root = Path::new(text(&result, "path"));
    let file = match language {
        "rust" => root.join("server/src/main.rs"),
        "typescript" => root.join("server/server.mjs"),
        _ => root.join("server/server.py"),
    };
    std::fs::write(&file, source)?;
    if language == "typescript" {
        std::fs::write(root.join("server/server.ts"), source)?;
    }
    let inspected = crate::plugins::inspect(root, "generated-draft")?;
    Ok(
        json!({"path":root,"draft":true,"plugin":inspected,"testSuggestions":authored["testSuggestions"],"usage":generation.usage,"diagnostics":["Generated source has not been executed. Review source, explicitly build/install, then exercise its real advertised tools and save test assertions."]}),
    )
}
pub(super) fn save_case(app: &App, mut value: Value) -> Result<Value> {
    if text(&value, "name").trim().is_empty() {
        bail!("Name the test case");
    }
    if value["method"] != "tools/call"
        && value["method"] != "resources/read"
        && value["method"] != "prompts/get"
    {
        bail!("Choose a testable MCP operation");
    }
    if value["params"] != redact(&value["params"], &[]) {
        bail!("Do not save credentials in test cases; configure host authentication instead");
    }
    if !value["expect"].is_object() {
        bail!("Expected results must be a JSON object");
    }
    if value["expect"]["pointer"].is_string() && value["expect"].get("equals").is_none() {
        bail!("A JSON-pointer assertion requires an explicit equals value");
    }
    let case = json!({"id":if text(&value,"id").is_empty(){id()}else{text(&value,"id").into()},"name":value["name"],"pluginId":value["pluginId"],"serverName":value["serverName"],"method":value["method"],"params":value["params"],"expect":value["expect"],"createdAt":now()});
    value = case;
    app.store.put("studio_case", &value)?;
    Ok(value)
}
pub(super) async fn run_cases(app: Arc<App>, value: &Value) -> Result<Value> {
    confirm(value)?;
    let mut results = vec![];
    for mut case in app
        .store
        .all("studio_case")?
        .into_iter()
        .filter(|c| text(value, "id").is_empty() || c["id"] == value["id"])
        .filter(|c| text(value, "pluginId").is_empty() || c["pluginId"] == value["pluginId"])
    {
        case["confirmed"] = json!(true);
        let response = invoke(app.clone(), &case).await;
        let actual = match response {
            Ok(v) => v,
            Err(e) => json!({"ok":false,"error":e.to_string()}),
        };
        let passed = expectation(&actual, &case["expect"]);
        case["lastResult"] = json!({"passed":passed,"actual":actual,"checkedAt":now()});
        case.as_object_mut().unwrap().remove("confirmed");
        app.store.put("studio_case", &case)?;
        results.push(json!({"id":case["id"],"name":case["name"],"passed":passed,"actual":actual}));
    }
    if results.is_empty() {
        bail!("No matching saved test cases");
    }
    Ok(json!({"passed":results.iter().all(|r|r["passed"]==true),"results":results}))
}
fn expectation(actual: &Value, expect: &Value) -> bool {
    let expect_ok = expect["ok"].as_bool().unwrap_or(true);
    if actual["ok"].as_bool().unwrap_or(false) != expect_ok {
        return false;
    }
    if let Some(contains) = expect["contains"].as_str()
        && !actual.to_string().contains(contains)
    {
        return false;
    }
    if let Some(pointer) = expect["pointer"].as_str() {
        return expect.get("equals").is_some_and(|expected| {
            actual
                .pointer(pointer)
                .is_some_and(|found| found == expected)
        });
    }
    true
}

fn repo_parts(value: &str) -> Result<(&str, &str)> {
    let value = value
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_end_matches('/');
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("").trim_end_matches(".git");
    if owner.is_empty()
        || repo.is_empty()
        || parts.next().is_some()
        || ![owner, repo].iter().all(|p| {
            p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
    {
        bail!("Enter a GitHub repository as owner/name");
    }
    Ok((owner, repo))
}
async fn release(app: &App, repo: &str, tag: &str) -> Result<Value> {
    let (owner, name) = repo_parts(repo)?;
    let mut url = url::Url::parse(&format!(
        "https://api.github.com/repos/{owner}/{name}/releases/"
    ))?;
    if tag == "latest" {
        url.path_segments_mut().unwrap().push("latest");
    } else {
        url.path_segments_mut().unwrap().push("tags").push(tag);
    }
    let response = app
        .client
        .get(url)
        .header("User-Agent", "Workbench")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("GitHub release lookup returned {}", response.status());
    }
    Ok(response.json().await?)
}
fn digest(value: &str) -> Result<String> {
    let s = value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .to_ascii_lowercase();
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("A complete SHA-256 digest is required");
    }
    Ok(s)
}
pub(super) async fn catalog_browse(app: &App, value: &Value) -> Result<Value> {
    let custom = text(value, "url");
    let address = if custom.is_empty() {
        "https://raw.githubusercontent.com/RobertIonutF/Workbench/main/catalog/registry.json"
    } else {
        custom
    };
    let mut diagnostics = vec![];
    let fetched = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        download(app, address, 512 * 1024),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("Registry lookup timed out")));
    let (registry, source) = match fetched {
        Ok(bytes) => (
            serde_json::from_slice::<Value>(&bytes)?,
            address.to_string(),
        ),
        Err(error) if custom.is_empty() => {
            diagnostics.push(format!(
                "Live catalog is unavailable; showing the bundled registry: {error}"
            ));
            (
                serde_json::from_str(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../catalog/registry.json"
                )))?,
                "bundled registry".into(),
            )
        }
        Err(error) => return Err(error),
    };
    let entries = registry
        .as_array()
        .or_else(|| registry["plugins"].as_array())
        .or_else(|| registry["entries"].as_array())
        .context("Registry needs a plugins array")?;
    let mut valid = vec![];
    for entry in entries.iter().take(200) {
        let accepted = repo_parts(text(entry, "repository")).is_ok()
            && !text(entry, "tag").is_empty()
            && text(entry, "tag") != "latest"
            && text(entry, "asset").ends_with(".zip")
            && digest(text(entry, "sha256")).is_ok();
        if accepted {
            valid.push(json!({"name":entry["name"],"description":entry["description"],"repository":entry["repository"],"tag":entry["tag"],"asset":entry["asset"],"sha256":text(entry,"sha256").trim_start_matches("sha256:").to_lowercase()}));
        } else {
            diagnostics.push(format!(
                "Skipped invalid registry entry {}",
                text(entry, "name")
            ));
        }
    }
    Ok(json!({"entries":valid,"source":source,"diagnostics":diagnostics}))
}
pub(super) async fn catalog_add(app: &App, value: &Value) -> Result<Value> {
    let repo = text(value, "repository");
    repo_parts(repo)?;
    let tag = text(value, "tag");
    if tag.is_empty() || tag == "latest" {
        bail!("Pin an exact release tag");
    }
    let release = release(app, repo, tag).await?;
    let asset = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == value["asset"]))
        .context("ZIP asset not found in that release")?;
    if !text(asset, "name").ends_with(".zip") {
        bail!("Choose a plugin ZIP release asset");
    }
    let hash = digest(if text(value, "sha256").is_empty() {
        text(asset, "digest")
    } else {
        text(value, "sha256")
    })?;
    let entry = json!({"id":id(),"repository":repo,"tag":release["tag_name"],"asset":asset["name"],"sha256":hash,"url":asset["browser_download_url"],"description":value["description"],"publisher":repo_parts(repo)?.0,"createdAt":now()});
    app.store.put("studio_catalog", &entry)?;
    Ok(entry)
}
pub(super) async fn catalog_check(app: &App, value: &Value) -> Result<Value> {
    let mut entry = app.store.get("studio_catalog", text(value, "id"))?;
    let latest = release(app, text(&entry, "repository"), "latest").await?;
    entry["latestTag"] = latest["tag_name"].clone();
    entry["updateAvailable"] = json!(entry["tag"] != latest["tag_name"]);
    entry["checkedAt"] = json!(now());
    app.store.put("studio_catalog", &entry)?;
    Ok(entry)
}
pub(super) async fn catalog_install(app: &App, value: &Value) -> Result<Value> {
    confirm(value)?;
    let entry = app.store.get("studio_catalog", text(value, "id"))?;
    let bytes = download(app, text(&entry, "url"), 64 * 1024 * 1024).await?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != text(&entry, "sha256") {
        bail!("Release asset digest changed; installation refused");
    }
    let path = app
        .data_dir
        .join("catalog-downloads")
        .join(format!("{}.zip", text(&entry, "id")));
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, bytes)?;
    let mut plugin = crate::plugins::install(app, &path, None)?;
    plugin["enabled"] = json!(false);
    plugin["catalog"] = entry;
    app.store.put("plugin", &plugin)?;
    Ok(
        json!({"plugin":plugin,"diagnostics":["Installed the digest-pinned package without executing it. Review its skills, server commands and permissions, then enable it."]}),
    )
}
pub(super) async fn download(app: &App, address: &str, limit: usize) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    let mut url = url::Url::parse(address)?;
    for _ in 0..6 {
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            bail!("Artifact downloads require credential-free HTTPS URLs");
        }
        let response = app
            .client
            .get(url.clone())
            .header("User-Agent", "Workbench")
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;
        if response.status().is_redirection() {
            url = url.join(
                response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .context("Redirect missing location")?
                    .to_str()?,
            )?;
            continue;
        }
        if !response.status().is_success() {
            bail!("Download returned {}", response.status());
        }
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            bail!("Artifact exceeds the download limit");
        }
        let mut stream = response.bytes_stream();
        let mut bytes = vec![];
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len() + chunk.len() > limit {
                bail!("Artifact exceeds the download limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    bail!("Too many download redirects")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expected_results_are_real_assertions() {
        let v = json!({"ok":true,"result":{"sum":4}});
        assert!(expectation(
            &v,
            &json!({"pointer":"/result/sum","equals":4})
        ));
        assert!(!expectation(
            &v,
            &json!({"pointer":"/result/sum","equals":5})
        ));
        assert!(!expectation(&v, &json!({"contains":"missing"})));
        assert!(!expectation(&v, &json!({"pointer":"/missing"})));
        assert!(!expectation(
            &v,
            &json!({"pointer":"/missing","equals":null})
        ));
    }
    #[test]
    fn repository_and_hash_validation() {
        assert!(repo_parts("owner/name").is_ok());
        assert!(repo_parts("https://evil.test/a/b").is_err());
        assert!(digest("short").is_err());
    }
    #[tokio::test]
    async fn catalog_uses_real_bundled_entries_when_network_is_unavailable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::new(Some(directory.path().join("data"))).await?;
        Arc::get_mut(&mut app).unwrap().client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(10))
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9")?)
            .build()?;
        let catalog = catalog_browse(&app, &json!({})).await?;
        assert_eq!(catalog["source"], "bundled registry");
        assert!(!catalog["entries"].as_array().unwrap().is_empty());
        assert!(!catalog["diagnostics"].as_array().unwrap().is_empty());
        for entry in catalog["entries"].as_array().unwrap() {
            assert_eq!(text(entry, "sha256").len(), 64);
            assert_ne!(entry["tag"], "latest");
        }
        assert!(
            catalog_browse(
                &app,
                &json!({"url":"http://untrusted.example/registry.json"})
            )
            .await
            .is_err()
        );
        Ok(())
    }
}
