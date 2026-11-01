//! Explicit, user-driven plugin development, local browser checks and profile maintenance.
use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Result, bail};
use axum::http::Method;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

#[path = "studio_backup.rs"]
mod backup;
#[path = "studio_browser.rs"]
mod browser;
#[path = "studio_plugins.rs"]
mod plugin;
#[path = "studio_updates.rs"]
mod updates;

pub fn state(app: &App) -> Result<Value> {
    Ok(
        json!({"cases":app.store.all("studio_case")?,"traces":app.store.all("studio_trace")?.into_iter().rev().take(80).collect::<Vec<_>>(),"catalog":app.store.all("studio_catalog")?,"previews":browser::state(app)?,"browserTests":app.store.all("studio_browser_test")?,"backups":app.store.all("studio_backup")?,"updates":app.store.all("studio_update")?}),
    )
}

pub async fn dispatch(
    app: Arc<App>,
    method: &Method,
    path: &str,
    _q: &HashMap<String, String>,
    value: Value,
) -> Option<Result<Value>> {
    if !path.starts_with("studio/") {
        return None;
    }
    let post = *method == Method::POST;
    Some(
        async {
            match (path, post) {
                ("studio/state", false) if *method == Method::GET => state(&app),
                ("studio/invoke", true) => plugin::invoke(app, &value).await,
                ("studio/generate", true) => plugin::generate(app, &value).await,
                ("studio/case", true) => plugin::save_case(&app, value),
                ("studio/case/delete", true) => {
                    app.store.delete("studio_case", text(&value, "id"))?;
                    Ok(json!({"ok":true}))
                }
                ("studio/cases/run", true) => plugin::run_cases(app, &value).await,
                ("studio/catalog/add", true) => plugin::catalog_add(&app, &value).await,
                ("studio/catalog/browse", true) => plugin::catalog_browse(&app, &value).await,
                ("studio/catalog/check", true) => plugin::catalog_check(&app, &value).await,
                ("studio/catalog/install", true) => plugin::catalog_install(&app, &value).await,
                ("studio/catalog/delete", true) => {
                    app.store.delete("studio_catalog", text(&value, "id"))?;
                    Ok(json!({"ok":true}))
                }
                ("studio/browser/start", true) => browser::start(app, &value).await,
                ("studio/browser/stop", true) => browser::stop(&app, &value),
                ("studio/browser/test", true) => browser::test(app, &value).await,
                ("studio/browser/artifact", true) => browser::artifact(&app, &value),
                ("studio/backup/create", true) => backup::create(app, &value).await,
                ("studio/backup/restore", true) => backup::restore(app, &value).await,
                ("studio/update/check", true) => updates::check(&app, &value).await,
                ("studio/update/install", true) => updates::install(&app, &value).await,
                _ => bail!("Unknown Studio operation"),
            }
        }
        .await,
    )
}

pub fn apply_pending_restore(data_dir: &std::path::Path) -> Result<()> {
    backup::apply_pending(data_dir)
}
pub async fn apply_update_after_exit(pid: u32, path: &std::path::Path, hash: &str) -> Result<()> {
    updates::apply_after_exit(pid, path, hash).await
}

fn confirm(value: &Value) -> Result<()> {
    if value["confirmed"] != true {
        bail!("Review this operation and explicitly confirm it before execution");
    }
    Ok(())
}
fn idle(app: &App) -> Result<()> {
    if !app.runs.lock().is_empty() {
        bail!("Finish or cancel running tasks and previews first");
    }
    Ok(())
}
pub(crate) fn redact(value: &Value, secrets: &[String]) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let sensitive = [
                        "authorization",
                        "api_key",
                        "apikey",
                        "access_token",
                        "refresh_token",
                        "id_token",
                        "client_secret",
                        "token",
                        "password",
                        "secret",
                        "secret_key",
                        "private_key",
                    ]
                    .contains(&k.to_ascii_lowercase().as_str());
                    (
                        k.clone(),
                        if sensitive {
                            json!("[redacted]")
                        } else {
                            redact(v, secrets)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|v| redact(v, secrets)).collect()),
        Value::String(s) => {
            let mut s = s.clone();
            for secret in secrets {
                if !secret.is_empty() {
                    s = s.replace(secret, "[redacted]");
                }
            }
            json!(s)
        }
        _ => value.clone(),
    }
}
fn trace(app: &App, mut value: Value) -> Result<()> {
    fn limit(value: &mut Value) {
        match value {
            Value::String(s) if s.len() > 16_000 => {
                let count = s.len();
                *s = format!(
                    "{}\n[Trace field truncated from {count} bytes]",
                    s.chars().take(12_000).collect::<String>()
                );
            }
            Value::Array(a) => {
                a.truncate(128);
                for item in a {
                    limit(item);
                }
            }
            Value::Object(m) => {
                for (k, item) in m {
                    if ["blob", "data"].contains(&k.as_str())
                        && item.as_str().is_some_and(|s| s.len() > 4096)
                    {
                        *item = json!("[Binary or large data field omitted from saved trace]");
                    } else {
                        limit(item);
                    }
                }
            }
            _ => {}
        }
    }
    limit(&mut value);
    value["id"] = json!(id());
    value["createdAt"] = json!(now());
    app.store.put("studio_trace", &value)?;
    let items = app.store.all("studio_trace")?;
    for old in items.iter().take(items.len().saturating_sub(200)) {
        app.store.delete("studio_trace", text(old, "id"))?;
    }
    Ok(())
}
pub(crate) fn known_secrets(app: &App) -> Vec<String> {
    fn collect(value: &Value, output: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (name, value) in map {
                    if [
                        "access_token",
                        "refresh_token",
                        "id_token",
                        "api_key",
                        "apikey",
                        "token",
                        "password",
                        "client_secret",
                        "authorization",
                        "secret",
                        "secret_key",
                        "private_key",
                    ]
                    .contains(&name.to_ascii_lowercase().as_str())
                    {
                        if let Some(s) = value.as_str() {
                            output.push(s.into());
                            if s.get(..7)
                                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
                            {
                                output.push(s[7..].into());
                            }
                        } else {
                            collect(value, output);
                        }
                    } else {
                        collect(value, output);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect(item, output);
                }
            }
            _ => {}
        }
    }
    let mut output = vec![app.token.clone()];
    #[cfg(windows)]
    if let Ok(files) = std::fs::read_dir(app.data_dir.join("vault")) {
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|s| s == "bin") {
                let name = path.file_stem().unwrap().to_string_lossy();
                if name == "runtime" {
                    continue;
                }
                if let Ok(secret) = crate::auth::get_secret(app, &name) {
                    if let Ok(value @ (Value::Object(_) | Value::Array(_))) =
                        serde_json::from_str::<Value>(&secret)
                    {
                        collect(&value, &mut output);
                    } else {
                        output.push(secret);
                    }
                }
            }
        }
    }
    #[cfg(not(windows))]
    for connection in app.store.all("connection").unwrap_or_default() {
        for name in [
            text(&connection, "id").to_string(),
            format!("{}-oauth", text(&connection, "id")),
        ] {
            if let Ok(secret) = crate::auth::get_secret(app, &name) {
                if let Ok(value @ (Value::Object(_) | Value::Array(_))) =
                    serde_json::from_str::<Value>(&secret)
                {
                    collect(&value, &mut output);
                } else {
                    output.push(secret);
                }
            }
        }
    }
    output.retain(|value| !value.is_empty());
    output.sort_by_key(|value| std::cmp::Reverse(value.len()));
    output.dedup();
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_masks_short_and_nested_credentials() {
        let redacted = redact(
            &json!({"message":"uses xy and a-long-secret", "nested":{"PRIVATE_KEY":"hidden"},"safe":2}),
            &["xy".into(), "a-long-secret".into()],
        );
        assert_eq!(redacted["message"], "uses [redacted] and [redacted]");
        assert_eq!(redacted["nested"]["PRIVATE_KEY"], "[redacted]");
        assert_eq!(redacted["safe"], 2);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn vault_collection_includes_numeric_keys_and_nested_bearer_tokens() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let app = App::new(Some(directory.path().into())).await?;
        crate::auth::set_secret(&app, "test-api", "123456")?;
        crate::auth::set_secret(
            &app,
            "test-oauth",
            &json!({"credentials":{"authorization":"bearer nested-token", "refresh_token":"refresh-token"},"unrelated":"public metadata"}).to_string(),
        )?;
        let secrets = known_secrets(&app);
        assert!(secrets.contains(&app.token));
        for expected in [
            "123456",
            "bearer nested-token",
            "nested-token",
            "refresh-token",
        ] {
            assert!(secrets.iter().any(|s| s == expected), "missing {expected}");
        }
        assert!(!secrets.iter().any(|s| s == "public metadata"));
        Ok(())
    }
}
