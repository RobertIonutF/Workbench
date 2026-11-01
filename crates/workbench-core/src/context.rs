use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn project_root(app: &App, project: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(text(
        &app.store.get("project", project)?,
        "path",
    )))
}
pub fn index(app: &App, project: &str, query: &str) -> Result<Value> {
    let root = project_root(app, project)?;
    let query = query.to_lowercase();
    let mut files = vec![];
    let mut symbols = vec![];
    for entry in ignore::WalkBuilder::new(&root)
        .hidden(false)
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some(".git" | "node_modules" | "target" | ".workbench-data")
            )
        })
        .build()
        .filter_map(Result::ok)
        .take(10000)
    {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(&root) else {
            continue;
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        if crate::workspace::resolve(&root, &path, false).is_err() {
            continue;
        }
        if query.is_empty() || path.to_lowercase().contains(&query) {
            files.push(json!({"path":path}));
        }
        if symbols.len() < 1000
            && let Ok(content) = crate::workspace::read(&root, &path)
        {
            for (line_number, line) in content.lines().enumerate() {
                let trimmed = line
                    .trim()
                    .trim_start_matches("export ")
                    .trim_start_matches("pub ")
                    .trim_start_matches("async ");
                for prefix in [
                    "fn ",
                    "function ",
                    "class ",
                    "struct ",
                    "enum ",
                    "interface ",
                    "type ",
                    "def ",
                    "trait ",
                ] {
                    if let Some(rest) = trimmed.strip_prefix(prefix) {
                        let name: String = rest
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty()
                            && (query.is_empty() || name.to_lowercase().contains(&query))
                        {
                            symbols.push(json!({"name":name,"path":path,"line":line_number+1,"kind":prefix.trim(),"indexKind":"lexical declaration"}));
                        }
                    }
                }
            }
        }
        if files.len() >= 1000 {
            break;
        }
    }
    Ok(json!({"files":files,"symbols":symbols,"truncated":files.len()>=1000||symbols.len()>=1000}))
}
pub fn save(app: &App, value: Value) -> Result<Value> {
    let kind = text(&value, "kind");
    let mut record = json!({"id":id(),"projectId":value["projectId"],"chatId":value["chatId"],"kind":kind,"name":value["name"],"path":value["path"],"createdAt":now()});
    match kind {
        "file" | "folder" | "symbol" => {
            let root = project_root(app, text(&value, "projectId"))?;
            crate::workspace::resolve(&root, text(&value, "path"), false)?;
            record["symbol"] = value["symbol"].clone();
            record["line"] = value["line"].clone();
        }
        "selection" => {
            if text(&value, "content").len() > 128000 {
                bail!("Selection exceeds 128 KiB");
            }
            record["content"] = value["content"].clone();
        }
        "image" | "pdf" => {
            let bytes = STANDARD
                .decode(text(&value, "dataBase64"))
                .context("Invalid attachment encoding")?;
            if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
                bail!("Attachments must be between 1 byte and 8 MiB");
            }
            if kind == "pdf" {
                if !bytes.starts_with(b"%PDF-") {
                    bail!("This attachment is not a PDF");
                }
                let extracted = pdf_extract::extract_text_from_mem(&bytes)
                    .context("PDF text extraction failed (scanned documents require OCR)")?;
                record["content"] = json!(extracted.chars().take(128000).collect::<String>());
                record["mime"] = json!("application/pdf");
            } else {
                let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    "image/png"
                } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                    "image/jpeg"
                } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
                    "image/webp"
                } else {
                    bail!("Only PNG, JPEG and WebP images are supported");
                };
                record["dataBase64"] = json!(STANDARD.encode(bytes));
                record["mime"] = json!(mime);
            }
        }
        _ => bail!("Unsupported context kind"),
    }
    let preview = resolve(app, &record)?;
    record["estimatedTokens"] = json!(text(&preview, "content").len().div_ceil(4));
    app.store.put("attachment", &record)?;
    let mut public = record;
    public.as_object_mut().unwrap().remove("dataBase64");
    Ok(public)
}
pub fn resolve(app: &App, item: &Value) -> Result<Value> {
    let mut result = item.clone();
    if matches!(text(item, "kind"), "file" | "symbol" | "folder") {
        let root = project_root(app, text(item, "projectId"))?;
        let path = text(item, "path");
        let content = if item["kind"] == "folder" {
            let directory = crate::workspace::resolve(&root, path, false)?;
            let mut joined = String::new();
            let mut count = 0;
            for entry in ignore::WalkBuilder::new(directory)
                .hidden(false)
                .filter_entry(|e| {
                    !matches!(
                        e.file_name().to_str(),
                        Some(".git" | "node_modules" | "target")
                    )
                })
                .build()
                .filter_map(Result::ok)
                .take(10000)
            {
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(&root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if let Ok(content) = crate::workspace::read(&root, &relative) {
                    if joined.len() + content.len() > 128000 {
                        break;
                    }
                    joined.push_str(&format!("\n--- {relative} ---\n{content}\n"));
                    count += 1;
                }
                if count >= 20 {
                    joined.push_str("\n[Folder context capped at 20 text files]\n");
                    break;
                }
            }
            joined
        } else {
            let content = crate::workspace::read(&root, path)?;
            if item["kind"] == "symbol" {
                let line = item["line"].as_u64().unwrap_or(1) as usize;
                content
                    .lines()
                    .skip(line.saturating_sub(6))
                    .take(100)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                content
            }
        };
        result["content"] = json!(content.chars().take(128000).collect::<String>());
    }
    Ok(result)
}
pub fn expand(app: &App, chat: &Value) -> Result<(String, Vec<Value>, Value)> {
    let mut context = String::new();
    let mut images = vec![];
    let mut sources = vec![];
    let mut scope = vec![text(chat, "projectId").to_owned()];
    let mut parent = text(chat, "projectId").to_owned();
    for _ in 0..8 {
        let Some(project) = app.store.get("project", &parent).ok() else {
            break;
        };
        let next = text(&project, "sourceProjectId");
        if next.is_empty() || scope.iter().any(|p| p == next) {
            break;
        }
        parent = next.to_owned();
        scope.push(parent.clone());
    }
    for attachment_id in chat["contextIds"].as_array().unwrap_or(&vec![]) {
        let mut item = app.store.get(
            "attachment",
            attachment_id.as_str().context("Invalid attachment ID")?,
        )?;
        if !text(&item, "projectId").is_empty()
            && !scope.iter().any(|p| p == text(&item, "projectId"))
        {
            bail!("Context belongs to a different project");
        }
        if matches!(text(&item, "kind"), "file" | "folder" | "symbol") {
            item["projectId"] = chat["projectId"].clone();
        }
        let resolved = resolve(app, &item)?;
        sources.push(json!({"id":item["id"],"name":item["name"],"path":item["path"],"kind":item["kind"],"estimatedTokens":text(&resolved,"content").len().div_ceil(4)}));
        if item["kind"] == "image" {
            images.push(json!({"mime":item["mime"],"dataBase64":item["dataBase64"]}));
        } else {
            context.push_str(&format!(
                "\n<attached-context source={:?} path={:?}>\n{}\n</attached-context>\n",
                text(&item, "name"),
                text(&item, "path"),
                text(&resolved, "content")
            ));
        }
        if context.len() > 256000 || images.len() > 5 {
            bail!("Selected attachments exceed 256 KiB text or 5 images; reduce context");
        }
    }
    let mut memory = vec![];
    for item in app.store.all("memory")? {
        if scope.iter().any(|p| p == text(&item, "projectId")) && item["enabled"] != false {
            let note = text(&item, "content");
            if context.len() + note.len() > 256000 {
                break;
            }
            context.push_str(&format!(
                "\n<project-memory id={:?}>\n{note}\n</project-memory>\n",
                text(&item, "id")
            ));
            memory.push(item["id"].clone());
        }
    }
    Ok((
        context,
        images,
        json!({"sources":sources,"memoryIds":memory,"estimateMethod":"UTF-8 bytes / 4; approximate"}),
    ))
}
pub fn mentions(app: &App, chat: &Value, input: &str) -> Result<String> {
    if text(chat, "projectId").is_empty() {
        return Ok(String::new());
    }
    let root = project_root(app, text(chat, "projectId"))?;
    let mut output = String::new();
    for token in input.split_whitespace().filter_map(|s| s.strip_prefix('@')) {
        let token = token.trim_end_matches([',', ';', ')']);
        let (kind, path) = if let Some(p) = token.strip_prefix("file:") {
            ("file", p)
        } else if let Some(p) = token.strip_prefix("folder:") {
            ("folder", p)
        } else if let Some(name) = token.strip_prefix("symbol:") {
            let found = index(app, text(chat, "projectId"), name)?;
            if let Some(symbol) = found["symbols"].as_array().and_then(|a| a.first()) {
                let content = resolve(
                    app,
                    &json!({"kind":"symbol","projectId":chat["projectId"],"path":symbol["path"],"line":symbol["line"]}),
                )?;
                output.push_str(&format!(
                    "\n<mentioned-symbol>\n{}\n</mentioned-symbol>\n",
                    text(&content, "content")
                ));
            }
            continue;
        } else {
            ("file", token)
        };
        if Path::new(path).is_absolute() {
            continue;
        }
        if crate::workspace::resolve(&root, path, false).is_ok() {
            let item = resolve(
                app,
                &json!({"kind":kind,"projectId":chat["projectId"],"path":path}),
            )?;
            output.push_str(&format!(
                "\n<mentioned-file path={path:?}>\n{}\n</mentioned-file>\n",
                text(&item, "content")
            ));
        }
        if output.len() > 128000 {
            bail!("Mentioned context exceeds 128 KiB");
        }
    }
    Ok(output)
}

pub fn tool_media(value: &Value) -> (Value, Vec<Value>) {
    let mut result = value.clone();
    let mut images = vec![];
    for key in ["content", "contents"] {
        if let Some(blocks) = result[key].as_array_mut() {
            for block in blocks {
                let mime = text(block, "mimeType").to_owned();
                let encoded = if block["type"] == "image" {
                    text(block, "data")
                } else {
                    text(block, "blob")
                }
                .to_owned();
                if encoded.is_empty() {
                    continue;
                }
                if ["image/png", "image/jpeg", "image/webp"].contains(&mime.as_str())
                    && images.len() < 5
                    && let Ok(bytes) = STANDARD.decode(&encoded)
                    && bytes.len() <= 8 * 1024 * 1024
                {
                    images.push(json!({"mime":mime,"dataBase64":encoded}));
                    block["preview"] = json!(
                        "Inline image retained for user review and vision-enabled model context"
                    );
                    if let Some(map) = block.as_object_mut() {
                        map.remove("data");
                        map.remove("blob");
                    }
                } else if mime == "application/pdf"
                    && let Ok(bytes) = STANDARD.decode(&encoded)
                    && bytes.len() <= 8 * 1024 * 1024
                {
                    let text = pdf_extract::extract_text_from_mem(&bytes)
                        .map(|s| s.chars().take(64000).collect::<String>())
                        .unwrap_or_else(|e| format!("PDF text extraction failed: {e}"));
                    block["extractedText"] = json!(text);
                    if let Some(map) = block.as_object_mut() {
                        map.remove("data");
                        map.remove("blob");
                    }
                }
            }
        }
    }
    (result, images)
}
