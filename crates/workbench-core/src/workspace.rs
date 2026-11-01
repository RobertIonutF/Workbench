use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
pub const MAX_FILE: u64 = 2 * 1024 * 1024;

pub fn resolve(root: &Path, relative: &str, create: bool) -> Result<PathBuf> {
    let p = Path::new(relative);
    if p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
        || relative.contains(':')
        || relative.contains('\0')
    {
        bail!("Path must stay relative to the selected project")
    }
    if p.components().any(|c| {
        let s = c.as_os_str().to_string_lossy().to_lowercase();
        s == ".git"
            || s == ".workbench"
            || s == ".workbench-data"
            || (s.starts_with(".env") && !s.ends_with(".example") && !s.ends_with(".sample"))
            || s == "id_rsa"
            || s == "id_ed25519"
            || s.ends_with(".pem")
            || s.ends_with(".key")
            || s.ends_with(".p12")
    }) {
        bail!("This path is excluded from agent access")
    }
    let root = std::fs::canonicalize(root)?;
    let candidate = root.join(p);
    let mut existing = candidate.as_path();
    while !existing.exists() {
        existing = existing.parent().context("Path has no existing parent")?;
    }
    let resolved = std::fs::canonicalize(existing)?;
    if !resolved.starts_with(&root) {
        bail!("Path resolves outside the project")
    }
    if candidate.exists() {
        let actual = std::fs::canonicalize(&candidate)?;
        if !actual.starts_with(&root) {
            bail!("Path escapes the project")
        };
        Ok(actual)
    } else if create {
        Ok(candidate)
    } else {
        bail!("File does not exist")
    }
}
pub fn read(root: &Path, path: &str) -> Result<String> {
    let p = resolve(root, path, false)?;
    if std::fs::metadata(&p)?.len() > MAX_FILE {
        bail!("File exceeds the 2 MiB read limit; use search or a smaller file")
    };
    std::fs::read_to_string(p).context("File is not readable UTF-8 text")
}
pub fn list(root: &Path, path: &str) -> Result<Value> {
    let p = resolve(root, path, false)?;
    let mut entries = Vec::new();
    for e in std::fs::read_dir(p)?.take(1000) {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        if [".git", "node_modules", "target", ".env"].contains(&name.as_str()) {
            continue;
        };
        let meta = e.metadata()?;
        let rel = if path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", path.trim_end_matches('/'), name)
        };
        entries.push(json!({"name":name,"path":rel,"isDir":meta.is_dir(),"size":meta.len()}));
    }
    entries.sort_by_key(|v| {
        (
            !v["isDir"].as_bool().unwrap_or(false),
            v["name"].as_str().unwrap_or("").to_lowercase(),
        )
    });
    Ok(json!({"entries":entries,"path":path}))
}
pub fn write(
    app: &crate::App,
    chat: &str,
    root: &Path,
    path: &str,
    content: &str,
    expected: Option<&str>,
) -> Result<()> {
    if content.len() > MAX_FILE as usize {
        bail!("Write exceeds 2 MiB")
    };
    let p = resolve(root, path, true)?;
    let before = if p.exists() {
        Some(read(root, path)?)
    } else {
        None
    };
    if let Some(e) = expected
        && before.as_deref() != Some(e)
    {
        bail!("File changed since it was read; refresh before writing")
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
        resolve(root, path, true)?;
    }
    let temporary = p.with_file_name(format!(".workbench-{}.tmp", crate::app::id()));
    std::fs::write(&temporary, content)?;
    if let Err(error) = std::fs::rename(&temporary, &p) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    if !chat.is_empty() {
        app.store
            .checkpoint(chat, path, before.as_deref(), content)?;
    }
    Ok(())
}
pub async fn diff(root: &Path) -> Result<String> {
    let repository = git2::Repository::open(root).context("This folder is not a Git repository")?;
    let mut options = git2::DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .max_size(MAX_FILE as i64);
    let changes = repository.diff_index_to_workdir(None, Some(&mut options))?;
    let mut output = String::new();
    let result = changes.print(git2::DiffFormat::Patch, |_, _, line| {
        if output.len() >= 1024 * 1024 {
            return false;
        }
        if matches!(line.origin(), '+' | '-' | ' ') {
            output.push(line.origin());
        }
        output.push_str(&String::from_utf8_lossy(line.content()));
        true
    });
    if output.len() >= 1024 * 1024 {
        output.push_str("\n[Diff truncated at 1 MiB]\n");
    } else {
        result?;
    }
    Ok(output)
}
pub fn status(root: &Path) -> Result<Value> {
    let repository = git2::Repository::open(root).context("This folder is not a Git repository")?;
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true)
        .update_index(false);
    let statuses = repository.statuses(Some(&mut options))?;
    let entries: Vec<Value> = statuses
        .iter()
        .take(1000)
        .map(|entry| json!({"path":entry.path(),"status":format!("{:?}",entry.status())}))
        .collect();
    Ok(json!({"entries":entries,"source":"libgit2","truncated":statuses.len()>1000}))
}
pub async fn review(app: &crate::App, project: &Value) -> Result<String> {
    let root = Path::new(crate::app::text(project, "path"));
    if git2::Repository::open(root).is_ok() {
        return diff(root).await;
    }
    let chats: Vec<Value> = app
        .store
        .all("chat")?
        .into_iter()
        .filter(|c| c["projectId"] == project["id"])
        .collect();
    let mut baselines = std::collections::BTreeMap::new();
    for (chat, path, before) in app.store.checkpoint_rows()? {
        if chats.iter().any(|c| c["id"] == chat) {
            baselines.entry(path).or_insert(before.unwrap_or_default());
        }
    }
    let mut output = String::new();
    for (path, before) in baselines {
        let after = read(root, &path).unwrap_or_default();
        let changes = similar::TextDiff::from_lines(&before, &after);
        output.push_str(
            &changes
                .unified_diff()
                .header(&format!("a/{path}"), &format!("b/{path}"))
                .to_string(),
        );
        if output.len() > 1024 * 1024 {
            output.push_str("\n[Diff truncated]\n");
            break;
        }
    }
    Ok(output)
}
pub fn undo(app: &crate::App, chat: &str, root: &Path, path: &str) -> Result<()> {
    let (before, after) = app.store.checkpoint_get(chat, path)?;
    if read(root, path)? != after {
        bail!("File has newer edits; automatic undo would overwrite them")
    };
    if let Some(before) = before {
        write(app, "", root, path, &before, Some(&after))?;
    } else {
        std::fs::remove_file(resolve(root, path, false)?)?;
    }
    app.store.checkpoint_delete(chat, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_escape_and_secrets() {
        let d = tempfile::tempdir().unwrap();
        assert!(resolve(d.path(), "../escape", true).is_err());
        assert!(resolve(d.path(), ".env", true).is_err());
        assert!(resolve(d.path(), "C:\\secrets", true).is_err());
        assert!(resolve(d.path(), "src/main.rs", true).is_ok());
    }
    #[tokio::test]
    async fn undo_preserves_user_edits() {
        let d = tempfile::tempdir().unwrap();
        let app = crate::App::new(Some(d.path().join("data"))).await.unwrap();
        std::fs::write(d.path().join("a.txt"), "old").unwrap();
        write(&app, "chat", d.path(), "a.txt", "agent", Some("old")).unwrap();
        std::fs::write(d.path().join("a.txt"), "user").unwrap();
        assert!(undo(&app, "chat", d.path(), "a.txt").is_err());
        assert_eq!(read(d.path(), "a.txt").unwrap(), "user");
    }
}
