use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && !name.contains("--")
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}
fn contained(root: &Path, path: &Path) -> Result<PathBuf> {
    let root = std::fs::canonicalize(root)?;
    let p = std::fs::canonicalize(path)?;
    if !p.starts_with(root) {
        bail!("Plugin member resolves outside its package")
    };
    Ok(p)
}
pub fn inspect(path: &Path, plugin_id: &str) -> Result<Value> {
    let root = std::fs::canonicalize(path)?;
    if !root.is_dir() {
        bail!("Plugin source must be a directory")
    };
    let mut diagnostics: Vec<String> = vec![];
    let (manifest_path, layout) = if root.join("plugin.json").is_file() {
        (root.join("plugin.json"), "portable")
    } else if root.join(".codex-plugin/plugin.json").is_file() {
        (root.join(".codex-plugin/plugin.json"), "codex")
    } else if root.join(".claude-plugin/plugin.json").is_file() {
        (root.join(".claude-plugin/plugin.json"), "claude")
    } else {
        bail!("No plugin.json, .codex-plugin/plugin.json or .claude-plugin/plugin.json found")
    };
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(contained(&root, &manifest_path)?)?)?;
    let name = text(&manifest, "name");
    if !valid_name(name) {
        bail!(
            "Plugin name must be 1–64 lowercase letters/numbers/dots/hyphens without repeated dots/hyphens"
        )
    }
    if layout == "portable"
        && manifest["$schema"] != "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"
    {
        bail!("Unsupported or missing portable manifest schema")
    }
    if layout == "portable" {
        let schema: Value = serde_json::from_str(include_str!("../schemas/plugin.schema.json"))?;
        let mut checked = manifest.clone();
        if let Some(fields) = checked.as_object_mut() {
            let known = schema["properties"].as_object().unwrap();
            let unknown: Vec<String> = fields
                .keys()
                .filter(|k| !known.contains_key(*k))
                .cloned()
                .collect();
            for key in unknown {
                fields.remove(&key);
                diagnostics.push(format!("Ignored unknown portable manifest field: {key}"));
            }
            // Unimplemented extension namespaces have no runtime authority in this host.
            fields.remove("extensions");
        }
        jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("Bundled schema error: {e}"))?
            .validate(&checked)
            .map_err(|e| anyhow::anyhow!("Invalid portable manifest: {e}"))?;
    }
    for key in [
        "hooks",
        "agents",
        "apps",
        "lspServers",
        "monitors",
        "commands",
        "outputStyles",
    ] {
        if !manifest[key].is_null() {
            diagnostics.push(format!(
                "{key}: host-specific component is not activated by Workbench"
            ));
        }
    }
    if layout != "portable" {
        diagnostics.push(format!("Imported {layout} compatibility layout; hosted connectors/accounts and executable hooks are not transferred."));
    }
    let skills_path = if layout != "portable" {
        manifest["skills"]
            .as_str()
            .map(|s| root.join(s))
            .unwrap_or(root.join("skills"))
    } else {
        root.join("skills")
    };
    let mut skills = vec![];
    if skills_path.exists() {
        if let Ok(path) = contained(&root, &skills_path) {
            if path.is_dir() {
                for child in std::fs::read_dir(path)? {
                    let child = child?;
                    let skillfile = child.path().join("SKILL.md");
                    if !skillfile.is_file() {
                        continue;
                    };
                    match parse_skill(&root,&skillfile){Ok((skill_name,description))=>skills.push(json!({"id":format!("{plugin_id}:{skill_name}"),"name":skill_name,"description":description,"path":skillfile.strip_prefix(&root)?.to_string_lossy()})),Err(error)=>diagnostics.push(format!("Skipped skill {}: {error}",child.file_name().to_string_lossy()))}
                }
            } else {
                diagnostics.push("skills location is not a directory".into())
            }
        } else {
            diagnostics.push("skills directory escapes package".into())
        }
    }
    let mcpfile = if layout == "portable" {
        root.join("mcp.json")
    } else {
        root.join(manifest["mcpServers"].as_str().unwrap_or(".mcp.json"))
    };
    let mut servers = vec![];
    if mcpfile.exists() {
        match load_servers(&root, &mcpfile, layout) {
            Ok((valid, warnings)) => {
                servers = valid;
                diagnostics.extend(warnings)
            }
            Err(e) => diagnostics.push(format!("MCP configuration disabled: {e}")),
        }
    }
    let mut digest = Sha256::new();
    for file in package_files(&root)? {
        let rel = file.strip_prefix(&root)?;
        digest.update(rel.to_string_lossy().as_bytes());
        digest.update(std::fs::read(file)?);
    }
    Ok(
        json!({"id":plugin_id,"name":name,"version":manifest["version"].as_str().unwrap_or("unversioned"),"description":manifest["description"].as_str().unwrap_or(""),"path":root.to_string_lossy(),"enabled":true,"source":root.to_string_lossy(),"skills":skills,"servers":servers,"diagnostics":diagnostics,"createdAt":now(),"digest":format!("{:x}",digest.finalize()),"layout":layout}),
    )
}
fn parse_skill(root: &Path, file: &Path) -> Result<(String, String)> {
    let file = contained(root, file)?;
    let raw = std::fs::read_to_string(&file)?;
    if raw.len() > 256 * 1024 {
        bail!("Skill exceeds size limit")
    };
    let normalized = raw.replace("\r\n", "\n");
    let content = normalized
        .strip_prefix("---\n")
        .context("Missing YAML frontmatter")?;
    let yaml = content
        .split_once("\n---")
        .context("Unclosed frontmatter")?
        .0;
    let front: serde_yaml::Value = serde_yaml::from_str(yaml)?;
    let name = front["name"].as_str().context("Missing skill name")?;
    let description = front["description"]
        .as_str()
        .context("Missing skill description")?;
    if !valid_name(name)
        || name.contains('.')
        || description.is_empty()
        || description.len() > 1024
        || file
            .parent()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy())
            != Some(name.into())
    {
        bail!("Invalid skill name/description or directory mismatch")
    };
    Ok((name.into(), description.into()))
}
fn load_servers(root: &Path, file: &Path, layout: &str) -> Result<(Vec<Value>, Vec<String>)> {
    let config: Value = serde_json::from_str(&std::fs::read_to_string(contained(root, file)?)?)?;
    if layout == "portable"
        && config["$schema"] != "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json"
    {
        bail!("Missing or unsupported MCP schema")
    };
    let map = config["mcpServers"]
        .as_object()
        .context("mcpServers must be an object")?;
    let mut servers = vec![];
    let mut warnings = vec![];
    let portable_schema: Value = serde_json::from_str(include_str!("../schemas/mcp.schema.json"))?;
    if layout == "portable"
        && config.as_object().is_none_or(|m| {
            m.keys()
                .any(|k| !["$schema", "mcpServers"].contains(&k.as_str()))
        })
    {
        bail!("Unknown top-level MCP configuration field");
    }
    let entry_schema = json!({"$ref":"#/$defs/server","$defs":portable_schema["$defs"]});
    let validator = jsonschema::validator_for(&entry_schema)
        .map_err(|e| anyhow::anyhow!("Bundled MCP schema: {e}"))?;
    for (name, server) in map {
        let mut server = server.clone();
        if layout != "portable" {
            if server["type"].is_null() {
                server["type"] = json!(if server["url"].is_string() {
                    "streamable-http"
                } else {
                    "stdio"
                });
            }
            if server["type"] == "http" {
                server["type"] = json!("streamable-http");
            }
            if let Some(args) = server["args"].as_array_mut() {
                for arg in args {
                    if let Some(s) = arg.as_str() {
                        *arg = json!(s.replace("${CLAUDE_PLUGIN_ROOT}", "${PLUGIN_ROOT}"));
                    }
                }
            }
        }
        let kind = text(&server, "type");
        if let Err(error) = validator.validate(&server) {
            warnings.push(format!("Skipped MCP {name}: {error}"));
            continue;
        }
        let valid = match kind {
            "stdio" => !text(&server, "command").is_empty(),
            "streamable-http" => url::Url::parse(text(&server, "url")).is_ok(),
            _ => false,
        };
        let allowed = if kind == "stdio" {
            vec!["type", "command", "args", "env", "cwd"]
        } else {
            vec!["type", "url", "headers"]
        };
        if !valid
            || server
                .as_object()
                .is_none_or(|m| m.keys().any(|k| !allowed.contains(&k.as_str())))
        {
            warnings.push(format!(
                "Skipped MCP {name}: unsupported transport, fields or missing command/URL"
            ));
            continue;
        }
        if text(&server, "command").starts_with("./")
            && root.join(text(&server, "command")).exists()
        {
            contained(root, &root.join(text(&server, "command")))?;
        }
        server["name"] = json!(name);
        servers.push(server);
    }
    Ok((servers, warnings))
}
pub fn package_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = vec![];
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some("node_modules" | "target" | ".git" | ".env" | "vault" | "__pycache__")
            )
        })
    {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            bail!("Symlinks are not allowed in packaged plugins")
        };
        if entry.file_type().is_file() {
            contained(root, entry.path())?;
            bytes += entry.metadata()?.len();
            files.push(entry.path().to_path_buf());
            if files.len() > 10000 || bytes > 128 * 1024 * 1024 {
                bail!("Plugin exceeds 10,000 files or 128 MiB")
            }
        }
    }
    files.sort();
    Ok(files)
}
fn copy_package(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for file in package_files(from)? {
        let destination = to.join(file.strip_prefix(from)?);
        std::fs::create_dir_all(destination.parent().unwrap())?;
        std::fs::copy(file, destination)?;
    }
    Ok(())
}
fn extract_zip(path: &Path, to: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    if archive.len() > 10000 {
        bail!("ZIP contains too many members")
    };
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file
            .enclosed_name()
            .context("ZIP path escapes destination")?;
        if name.components().any(|part| {
            let s = part.as_os_str().to_string_lossy();
            s.contains(':') || s.ends_with('.') || s.ends_with(' ')
        }) {
            bail!("ZIP contains an ambiguous Windows path");
        }
        if !seen.insert(name.to_string_lossy().to_lowercase()) {
            bail!("Duplicate ZIP entry")
        };
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            bail!("ZIP symlinks are not allowed")
        };
        total += file.size();
        if total > 128 * 1024 * 1024 || file.size() > 32 * 1024 * 1024 {
            bail!("ZIP exceeds package size limits")
        };
        let target = to.join(&name);
        if file.is_dir() {
            std::fs::create_dir_all(target)?;
        } else {
            std::fs::create_dir_all(target.parent().unwrap())?;
            let mut output = std::fs::File::create(target)?;
            let expected = file.size();
            let copied = std::io::copy(&mut (&mut file).take(32 * 1024 * 1024 + 1), &mut output)?;
            if copied != expected {
                bail!("ZIP member size differs from its directory metadata");
            }
        }
    }
    Ok(())
}
pub fn install(app: &App, path: &Path, existing: Option<&str>) -> Result<Value> {
    let installation_id = existing.map(str::to_owned).unwrap_or_else(id);
    let staging = app.data_dir.join("plugin-staging").join(id());
    std::fs::create_dir_all(&staging)?;
    let result = (|| {
        let source = if path.is_dir() {
            std::fs::canonicalize(path)?
        } else {
            extract_zip(path, &staging)?;
            if staging.join("plugin.json").exists()
                || staging.join(".codex-plugin").exists()
                || staging.join(".claude-plugin").exists()
            {
                staging.clone()
            } else {
                let directories = std::fs::read_dir(&staging)?
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .collect::<Vec<_>>();
                if directories.len() != 1 {
                    bail!("ZIP must contain one plugin root")
                };
                directories[0].path()
            }
        };
        let mut plugin = inspect(&source, &installation_id)?;
        let old = existing.and_then(|i| app.store.get("plugin", i).ok());
        if let Some(old) = &old {
            if old["name"] != plugin["name"] {
                bail!("An update must retain the plugin name")
            };
            if app
                .store
                .all("run")?
                .iter()
                .any(|r| r["status"] == "running")
            {
                bail!("Finish or cancel active runs before updating plugin data/versions")
            }
        }
        let destination = app
            .data_dir
            .join("plugins")
            .join(&installation_id)
            .join(text(&plugin, "digest"));
        if !destination.exists() {
            copy_package(&source, &destination)?;
        }
        plugin["path"] = json!(destination.to_string_lossy());
        plugin["source"] = json!(path.to_string_lossy());
        if let Some(old) = old {
            plugin["previousPath"] = old["path"].clone();
            plugin["enabled"] = json!(false);
            plugin["diagnostics"].as_array_mut().unwrap().push(json!(
                "Updated version is disabled until you review and enable it."
            ));
        }
        app.store.put("plugin", &plugin)?;
        Ok(plugin)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}
pub async fn build(app: &App, source: &Path) -> Result<Value> {
    let root = std::fs::canonicalize(source)?;
    inspect(&root, "authoring")?;
    let manifest = root.join("server/Cargo.toml");
    if !manifest.is_file() {
        return Ok(
            json!({"path":root,"diagnostics":["This template runs directly and does not need compilation."]}),
        );
    }
    let target = app.data_dir.join("plugin-builds").join(id());
    let args = vec![
        "build".into(),
        "--release".into(),
        "--manifest-path".into(),
        manifest.to_string_lossy().to_string(),
        "--target-dir".into(),
        target.to_string_lossy().to_string(),
    ];
    let result = crate::runtime::command_output(
        &root,
        "cargo",
        &args,
        600,
        &CancellationToken::new(),
        false,
    )
    .await?;
    if result["exitCode"] != 0 {
        bail!("Plugin build failed: {}", text(&result, "stderr"));
    }
    let binary = if cfg!(windows) {
        "workbench-plugin-server.exe"
    } else {
        "workbench-plugin-server"
    };
    let destination = root.join(if cfg!(windows) {
        "bin/server.exe"
    } else {
        "bin/server"
    });
    std::fs::create_dir_all(destination.parent().unwrap())?;
    std::fs::copy(target.join("release").join(binary), &destination)?;
    Ok(json!({"path":root,"diagnostics":[format!("Built {}",destination.display())]}))
}

pub fn pack(app: &App, plugin: &Value, output: &Path) -> Result<Value> {
    let root = Path::new(text(plugin, "path"));
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        app.data_dir.join("exports").join(output)
    };
    if output.exists() {
        bail!("Output already exists; choose a new filename")
    };
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&output)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for member in package_files(root)? {
        zip.start_file(
            member
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/"),
            options,
        )?;
        zip.write_all(&std::fs::read(member)?)?;
    }
    zip.finish()?;
    Ok(json!({"path":output.to_string_lossy()}))
}
pub async fn runtime_server(app: &App, plugin: &Value, server: &Value) -> Result<Value> {
    let mut config = server.clone();
    if server["type"] == "streamable-http" {
        config["_workbenchHeaders"] =
            crate::mcp_auth::headers(app, text(plugin, "id"), server).await?;
    }
    Ok(config)
}
pub async fn test(app: Arc<App>, plugin: &Value) -> Result<Value> {
    let mut diagnostics: Vec<String> = vec![];
    let mut all_tools = vec![];
    let cancel = CancellationToken::new();
    for server in plugin["servers"].as_array().unwrap_or(&vec![]) {
        let data = app
            .data_dir
            .join("plugin-data")
            .join(text(plugin, "id"))
            .join(text(server, "name"));
        let config = runtime_server(&app, plugin, server).await?;
        match crate::mcp::Session::connect(
            &config,
            Path::new(text(plugin, "path")),
            &data,
            app.client.clone(),
            &cancel,
        )
        .await
        {
            Ok(mut session) => {
                match session.tools(&cancel).await {
                    Ok(tools) => all_tools.extend(tools),
                    Err(e) => diagnostics.push(e.to_string()),
                };
                session.close().await;
            }
            Err(e) => diagnostics.push(format!("{}: {e}", text(server, "name"))),
        }
    }
    Ok(json!({"ok":diagnostics.is_empty(),"tools":all_tools,"diagnostics":diagnostics}))
}
pub fn skill_content(plugin: &Value, name: &str) -> Result<String> {
    let skill = plugin["skills"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["name"] == name))
        .context("Skill not found")?;
    let root = Path::new(text(plugin, "path"));
    Ok(std::fs::read_to_string(contained(
        root,
        &root.join(text(skill, "path")),
    )?)?)
}
pub fn create(value: &Value) -> Result<Value> {
    let name = text(value, "name");
    if !valid_name(name) || name.contains('.') {
        bail!("Choose a lowercase hyphenated plugin name")
    };
    let root = PathBuf::from(text(value, "path"));
    if !root.is_absolute() {
        bail!("Choose an absolute output folder")
    };
    if root.exists() && std::fs::read_dir(&root)?.next().is_some() {
        bail!("Choose an empty output directory")
    };
    std::fs::create_dir_all(&root)?;
    let description = text(value, "description");
    std::fs::write(
        root.join("plugin.json"),
        serde_json::to_string_pretty(
            &json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":name,"version":"0.1.0","description":description}),
        )?,
    )?;
    let template = text(value, "template");
    let language = text(value, "language");
    let existing = !text(value, "serverUrl").is_empty() || !text(value, "serverCommand").is_empty();
    let has_mcp = template == "mcp" || template == "combined" || existing;
    if template != "mcp" {
        let skill = format!("{name}-workflow");
        let dir = root.join("skills").join(&skill);
        std::fs::create_dir_all(&dir)?;
        let body = if text(value, "skillBody").is_empty() {
            if existing {
                "Discover this plugin's actual MCP capabilities and use the relevant tools for the user's request. Report real results, permissions needed and missing evidence. Do not invent tool names or successful actions."
            } else if has_mcp {
                "Use the plugin's calculate tool to add the requested numbers. Report the returned sum, and explain errors without inventing a result."
            } else {
                "Inspect the user's requested project using the available read-only tools. Return findings with file references, distinguish observations from assumptions, and describe any missing evidence."
            }
        } else {
            text(value, "skillBody")
        };
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {skill}\ndescription: {}\n---\n\n{body}\n",
                serde_json::to_string(if description.is_empty() {
                    "A reusable Workbench workflow."
                } else {
                    description
                })?
            ),
        )?;
    }
    let mut diagnostics: Vec<String> = vec![];
    if existing {
        let server = if !text(value, "serverUrl").is_empty() {
            let url = url::Url::parse(text(value, "serverUrl"))
                .context("Enter a complete MCP endpoint URL")?;
            if url.scheme() != "https"
                && !(url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")))
            {
                bail!("Remote MCP needs HTTPS; plain HTTP is allowed for loopback only");
            }
            json!({"type":"streamable-http","url":url.as_str()})
        } else {
            let args = value["serverArgs"].as_array().cloned().unwrap_or_default();
            if args.iter().any(|a| !a.is_string()) {
                bail!("Server arguments must be strings");
            }
            json!({"type":"stdio","command":value["serverCommand"],"args":args})
        };
        std::fs::write(
            root.join("mcp.json"),
            serde_json::to_string_pretty(
                &json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"server":server}}),
            )?,
        )?;
        diagnostics.push("Existing MCP connection wrapped without running it. Install and test to verify actual tools; configure remote credentials in the host.".into());
    } else if has_mcp {
        let server = root.join("server");
        std::fs::create_dir_all(&server)?;
        let (command, args) = match language {
            "rust" => {
                std::fs::create_dir_all(server.join("src"))?;
                std::fs::write(
                    server.join("Cargo.toml"),
                    "[package]\nname = \"workbench-plugin-server\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n[dependencies]\nserde_json = \"1\"\n",
                )?;
                std::fs::write(server.join("src/main.rs"), RUST_SERVER)?;
                diagnostics.push("Build with: cargo build --release --manifest-path server/Cargo.toml. Then copy server/target/release/workbench-plugin-server.exe into bin/server.exe before installation/packaging.".into());
                std::fs::create_dir_all(root.join("bin"))?;
                (
                    if cfg!(windows) {
                        "./bin/server.exe"
                    } else {
                        "./bin/server"
                    },
                    vec![],
                )
            }
            "typescript" => {
                std::fs::write(server.join("server.mjs"), JS_SERVER)?;
                std::fs::write(server.join("server.ts"), JS_SERVER)?;
                diagnostics.push("Requires Node.js 20+. The runnable .mjs companion uses Node built-ins and needs no npm installation.".into());
                ("node", vec!["${PLUGIN_ROOT}/server/server.mjs"])
            }
            _ => {
                std::fs::write(server.join("server.py"), PY_SERVER)?;
                diagnostics.push("Requires Python 3.10+. Uses the standard library only.".into());
                ("python", vec!["${PLUGIN_ROOT}/server/server.py"])
            }
        };
        std::fs::write(
            root.join("mcp.json"),
            serde_json::to_string_pretty(
                &json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"calculator":{"type":"stdio","command":command,"args":args}}}),
            )?,
        )?;
    }
    std::fs::write(
        root.join("README.md"),
        format!(
            "# {name}\n\n{description}\n\nOpen this folder in Workbench's plugin manager. Validate, build if required, install, then test. Local MCP servers run with the selected native authority; loading a skill alone launches nothing.\n"
        ),
    )?;
    std::fs::write(
        root.join("LICENSE"),
        "All rights reserved by the plugin author. Replace with your chosen license before distributing.\n",
    )?;
    Ok(json!({"path":root.to_string_lossy(),"diagnostics":diagnostics}))
}
const PY_SERVER: &str = r#"import sys,json
for line in sys.stdin:
    try:
        m=json.loads(line)
        if 'id' not in m: continue
        method=m.get('method'); p=m.get('params',{})
        if method=='initialize': result={'protocolVersion':'2025-11-25','capabilities':{'tools':{}},'serverInfo':{'name':'workbench-calculator','version':'0.1.0'}}
        elif method=='tools/list': result={'tools':[{'name':'calculate','description':'Add a list of numbers without external effects.','inputSchema':{'type':'object','properties':{'numbers':{'type':'array','items':{'type':'number'}}},'required':['numbers'],'additionalProperties':False}}]}
        elif method=='tools/call' and p.get('name')=='calculate': result={'content':[{'type':'text','text':json.dumps({'sum':sum(p['arguments']['numbers'])})}]}
        else: raise ValueError('Unsupported method')
        print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
    except Exception as e:
        print(json.dumps({'jsonrpc':'2.0','id':m.get('id'),'error':{'code':-32602,'message':str(e)}}),flush=True)
"#;
const JS_SERVER: &str = r#"import readline from 'node:readline';
for await (const line of readline.createInterface({input:process.stdin})) {
  let m; try { m=JSON.parse(line); if(m.id===undefined) continue; let result;
    if(m.method==='initialize') result={protocolVersion:'2025-11-25',capabilities:{tools:{}},serverInfo:{name:'workbench-calculator',version:'0.1.0'}};
    else if(m.method==='tools/list') result={tools:[{name:'calculate',description:'Add numbers without external effects.',inputSchema:{type:'object',properties:{numbers:{type:'array',items:{type:'number'}}},required:['numbers'],additionalProperties:false}}]};
    else if(m.method==='tools/call'&&m.params.name==='calculate') { const numbers=m.params.arguments.numbers; if(!Array.isArray(numbers)||numbers.some(n=>typeof n!=='number')) throw Error('numbers must be an array of numbers'); result={content:[{type:'text',text:JSON.stringify({sum:numbers.reduce((a,b)=>a+b,0)})}]}; }
    else throw Error('Unsupported method');
    console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,result}));
  } catch(e) {console.log(JSON.stringify({jsonrpc:'2.0',id:m?.id,error:{code:-32602,message:String(e)}}));}
}
"#;
const RUST_SERVER: &str = r#"use std::io::{self, BufRead, Write};
use serde_json::{Value, json};

fn result(message: &Value) -> Result<Value, (i32, &'static str)> {
    if message["jsonrpc"] != "2.0" {
        return Err((-32600, "Expected a JSON-RPC 2.0 request"));
    }
    match message["method"].as_str().ok_or((-32600, "Method must be a string"))? {
        "initialize" => Ok(json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"workbench-calculator","version":"0.1.0"}})),
        "tools/list" => Ok(json!({"tools":[{"name":"calculate","description":"Add numbers without external effects.","inputSchema":{"type":"object","properties":{"numbers":{"type":"array","items":{"type":"number"}}},"required":["numbers"],"additionalProperties":false}}]})),
        "tools/call" => {
            if message["params"]["name"] != "calculate" {
                return Err((-32602, "Unknown tool; use tools/list to discover available tools"));
            }
            let arguments = message["params"]["arguments"].as_object()
                .ok_or((-32602, "Arguments must be an object containing numbers"))?;
            if arguments.len() != 1 || !arguments.contains_key("numbers") {
                return Err((-32602, "Arguments must contain only the required numbers field"));
            }
            let numbers = arguments["numbers"].as_array()
                .ok_or((-32602, "numbers must be an array of finite numbers"))?;
            let mut sum = 0.0_f64;
            for number in numbers {
                let number = number.as_f64().filter(|value| value.is_finite())
                    .ok_or((-32602, "numbers must be an array of finite numbers"))?;
                sum += number;
                if !sum.is_finite() {
                    return Err((-32602, "The sum exceeds the supported numeric range"));
                }
            }
            Ok(json!({"content":[{"type":"text","text":json!({"sum":sum}).to_string()}]}))
        }
        _ => Err((-32601, "Method not found")),
    }
}

fn response(message: &Value) -> Value {
    match result(message) {
        Ok(result) => json!({"jsonrpc":"2.0","id":message["id"],"result":result}),
        Err((code, message_text)) => json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":code,"message":message_text}}),
    }
}

fn main() {
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => {
                if message.get("id").is_none() { continue; } // Notifications have no response.
                response(&message)
            }
            Err(_) => json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
        };
        println!("{reply}");
        if io::stdout().flush().is_err() { break; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(arguments: Value) -> Value {
        response(&json!({"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"calculate","arguments":arguments}}))
    }

    #[test]
    fn valid_fractional_numbers_return_the_actual_sum() {
        let reply = call(json!({"numbers":[1.25,2.75]}));
        let content: Value = serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(content["sum"], 4.0);
        assert_eq!(reply["id"], 42);
    }

    #[test]
    fn malformed_arguments_are_rejected_instead_of_coerced_to_zero() {
        for arguments in [json!({}), json!({"numbers":[1,"invalid"]}), json!({"numbers":[true]}), json!({"numbers":"bad"}), json!({"numbers":[],"extra":1}), Value::Null] {
            assert_eq!(call(arguments)["error"]["code"], -32602);
        }
    }

    #[test]
    fn unknown_tools_and_methods_return_protocol_errors() {
        let tool = response(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"unknown","arguments":{"numbers":[]}}}));
        assert_eq!(tool["error"]["code"], -32602);
        let method = response(&json!({"jsonrpc":"2.0","id":2,"method":"unknown/method"}));
        assert_eq!(method["error"]["code"], -32601);
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_package_and_skips_bad_skill() {
        let d = tempfile::tempdir().unwrap();
        create(&json!({"name":"sample-plugin","path":d.path(),"template":"skill","description":"Read projects"})).unwrap();
        let p = inspect(d.path(), "test").unwrap();
        assert_eq!(p["skills"].as_array().unwrap().len(), 1);
        std::fs::create_dir_all(d.path().join("skills/bad")).unwrap();
        std::fs::write(d.path().join("skills/bad/SKILL.md"), "bad").unwrap();
        let p = inspect(d.path(), "test").unwrap();
        assert_eq!(p["skills"].as_array().unwrap().len(), 1);
        assert_eq!(p["diagnostics"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn rejects_zip_traversal() {
        let d = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(d.path().join("bad.zip")).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("../escape.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"bad").unwrap();
        zip.finish().unwrap();
        assert!(extract_zip(&d.path().join("bad.zip"), &d.path().join("extract")).is_err());
    }
}
