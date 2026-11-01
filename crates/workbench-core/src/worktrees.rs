use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn create(app: &App, value: &Value) -> Result<Value> {
    let project = app.store.get("project", text(value, "projectId"))?;
    let repository = git2::Repository::open(text(&project, "path"))
        .context("Worktrees require a Git repository with a commit")?;
    let commit = repository.head()?.peel_to_commit()?;
    let identity = id();
    let branch_name = format!("workbench/{}", &identity[..12]);
    let reference = repository
        .branch(&branch_name, &commit, false)?
        .into_reference();
    let destination = app.data_dir.join("worktrees").join(&identity);
    std::fs::create_dir_all(destination.parent().unwrap())?;
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&reference));
    repository.worktree(&identity, &destination, Some(&options))?;
    let mut linked = app.project(
        json!({"path":destination,"name":format!("{} · {}",text(&project,"name"),&identity[..8])}),
    )?;
    linked["sourceProjectId"] = project["id"].clone();
    linked["worktreeId"] = json!(identity);
    app.store.put("project", &linked)?;
    let worktree = json!({"id":identity,"projectId":linked["id"],"sourceProjectId":project["id"],"path":linked["path"],"branch":branch_name,"baseCommit":commit.id().to_string(),"createdAt":now(),"baseline":"committed HEAD; original uncommitted files stay in their checkout"});
    app.store.put("worktree", &worktree)?;
    Ok(worktree)
}
fn repository(app: &App, project_id: &str) -> Result<git2::Repository> {
    git2::Repository::open(text(&app.store.get("project", project_id)?, "path"))
        .context("Select a Git project")
}
pub fn commit(app: &App, value: &Value) -> Result<Value> {
    let repo = repository(app, text(value, "projectId"))?;
    if text(value, "message").trim().is_empty() {
        bail!("Enter a commit message");
    }
    let root = repo
        .workdir()
        .context("Bare repositories are not supported")?;
    let mut index = repo.index()?;
    let paths = value["paths"]
        .as_array()
        .context("Choose the files to stage")?;
    if paths.is_empty() {
        bail!("Choose at least one file");
    }
    let head = repo.head()?.peel_to_tree()?;
    let staged = repo.diff_tree_to_index(Some(&head), Some(&index), None)?;
    for delta in staged.deltas() {
        if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path())
            && !paths
                .iter()
                .any(|p| p.as_str().is_some_and(|p| Path::new(p) == path))
        {
            bail!(
                "Other files are already staged; include or unstage them before committing selected paths"
            );
        }
    }
    for path in paths {
        let path = path.as_str().context("Invalid file path")?;
        let resolved = crate::workspace::resolve(root, path, true)?;
        if resolved.exists() {
            index.add_path(Path::new(path))?;
        } else {
            index.remove_path(Path::new(path))?;
        }
    }
    index.write()?;
    let tree = repo.find_tree(index.write_tree()?)?;
    let signature = repo
        .signature()
        .or_else(|_| git2::Signature::now("Workbench", "workbench@localhost"))?;
    let parent = repo.head()?.peel_to_commit()?;
    if parent.tree_id() == tree.id() {
        bail!("The selected files have no staged changes");
    }
    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        text(value, "message"),
        &tree,
        &[&parent],
    )?;
    Ok(json!({"commit":oid.to_string(),"branch":repo.head()?.shorthand()}))
}
pub fn merge(app: &App, worktree_id: &str) -> Result<Value> {
    let worktree = app.store.get("worktree", worktree_id)?;
    let repo = repository(app, text(&worktree, "sourceProjectId"))?;
    let source = git2::Repository::open(text(&worktree, "path"))?;
    if !source.statuses(None)?.is_empty() {
        bail!(
            "Commit reviewed changes in the task worktree before integration; no uncommitted edits were merged"
        );
    }
    if crate::tasks::project_writer_active(app, text(&worktree, "projectId"))?
        || crate::tasks::project_writer_active(app, text(&worktree, "sourceProjectId"))?
    {
        bail!("Finish active writers before integrating this worktree");
    }
    if !repo.statuses(None)?.is_empty() {
        bail!("Commit or stash changes in the destination checkout before integration");
    }
    let theirs = git2::Repository::open(text(&worktree, "path"))?
        .head()?
        .peel_to_commit()?
        .id();
    let ours = repo.head()?.peel_to_commit()?;
    let theirs = repo.find_commit(theirs)?;
    let ancestor = repo.find_commit(repo.merge_base(ours.id(), theirs.id())?)?;
    let mut index = repo.merge_trees(&ancestor.tree()?, &ours.tree()?, &theirs.tree()?, None)?;
    if index.has_conflicts() {
        bail!(
            "Integration has merge conflicts. Resolve them in the task worktree first; destination files were not changed"
        );
    }
    let tree = repo.find_tree(index.write_tree_to(&repo)?)?;
    let signature = repo
        .signature()
        .or_else(|_| git2::Signature::now("Workbench", "workbench@localhost"))?;
    repo.checkout_tree(
        tree.as_object(),
        Some(git2::build::CheckoutBuilder::new().safe()),
    )?;
    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &format!("Integrate {}", text(&worktree, "branch")),
        &tree,
        &[&ours, &theirs],
    )?;
    Ok(json!({"commit":oid.to_string(),"integrated":worktree["branch"]}))
}
pub async fn pull_request(app: &App, value: &Value) -> Result<Value> {
    if value["confirmPublish"] != true {
        bail!("Confirm publishing the selected branch and PR to its GitHub remote");
    }
    let (slug, branch, oid, objects) = {
        let repo = repository(app, text(value, "projectId"))?;
        let remote = repo.find_remote("origin")?;
        let address = remote.url().context("Origin has no URL")?;
        if remote.pushurl().is_some_and(|push| push != address) {
            bail!(
                "Origin has a different push destination; configure matching fetch/push URLs before publishing from Workbench"
            );
        }
        let slug = if let Some(path) = address.strip_prefix("https://github.com/") {
            path
        } else if let Some(path) = address.strip_prefix("git@github.com:") {
            path
        } else {
            bail!("PR publishing supports an explicit github.com origin");
        }
        .trim_end_matches(".git")
        .to_owned();
        if slug.split('/').count() != 2
            || slug.split('/').any(|part| {
                part.is_empty()
                    || !part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            })
        {
            bail!("Invalid GitHub repository origin");
        }
        let head = repo.head()?;
        let branch = head
            .shorthand()
            .context("Choose a named branch first")?
            .to_owned();
        let oid = head.peel_to_commit()?.id();
        let objects = std::fs::canonicalize(repo.commondir().join("objects"))?;
        (slug, branch, oid, objects)
    };
    if text(value, "title").is_empty() || text(value, "base").is_empty() {
        bail!("Enter the PR title and base branch");
    }
    if branch == text(value, "base") {
        bail!(
            "Switch to a feature branch before publishing a pull request against this base branch"
        );
    }
    let token = tokio_util::sync::CancellationToken::new();
    // Native Git sees a host-owned bare repository, never project-controlled
    // config, hooks, pushurl, URL rewrites, filters or credential helpers.
    let publish = app.data_dir.join("publishing").join(id());
    std::fs::create_dir_all(&publish)?;
    let neutral = git2::Repository::init_bare(&publish)?;
    std::fs::write(
        publish.join("config"),
        "[core]\nrepositoryformatversion=0\nbare=true\nfilemode=false\n",
    )?;
    let object_path = objects
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .replace('\\', "/");
    std::fs::create_dir_all(publish.join("objects/info"))?;
    std::fs::write(
        publish.join("objects/info/alternates"),
        format!("{object_path}\n"),
    )?;
    neutral.odb()?.add_disk_alternate(&object_path)?;
    neutral.reference(
        &format!("refs/heads/{branch}"),
        oid,
        false,
        "Workbench publication snapshot",
    )?;
    drop(neutral);
    let mut environment = vec![
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        (
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) {
                "NUL".into()
            } else {
                "/dev/null".into()
            },
        ),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        ("GH_HOST", "github.com".into()),
    ];
    for key in ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "GH_CONFIG_DIR"] {
        if let Ok(value) = std::env::var(key) {
            environment.push((key, value));
        }
    }
    let hooks = app.data_dir.join("no-git-hooks");
    std::fs::create_dir_all(&hooks)?;
    let pushed = crate::runtime::command_output_with_env(
        &app.data_dir,
        "git",
        &[
            format!("--git-dir={}", publish.display()),
            "-c".into(),
            format!("core.hooksPath={}", hooks.display()),
            "-c".into(),
            "credential.helper=".into(),
            "-c".into(),
            "credential.helper=!gh auth git-credential".into(),
            "push".into(),
            format!("https://github.com/{slug}.git"),
            format!("{oid}:refs/heads/{branch}"),
        ],
        120,
        &token,
        false,
        &environment,
    )
    .await?;
    if pushed["exitCode"] != 0 {
        bail!("Branch push failed: {}", text(&pushed, "stderr"));
    }
    let body = app.data_dir.join(format!("pr-body-{}.md", id()));
    std::fs::write(&body, text(value, "body"))?;
    let result = crate::runtime::command_output_with_env(
        &app.data_dir,
        "gh",
        &[
            "pr".into(),
            "create".into(),
            "--repo".into(),
            slug,
            "--head".into(),
            branch,
            "--base".into(),
            text(value, "base").into(),
            "--title".into(),
            text(value, "title").into(),
            "--body-file".into(),
            body.to_string_lossy().into(),
        ],
        120,
        &token,
        false,
        &environment,
    )
    .await;
    let _ = std::fs::remove_file(body);
    let result = result?;
    if result["exitCode"] != 0 {
        bail!(
            "PR creation failed after branch push: {}",
            text(&result, "stderr")
        );
    }
    Ok(json!({"url":text(&result,"stdout").trim(),"commit":oid.to_string()}))
}

fn hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}
pub fn review(app: &App, chat_id: &str, path: &str) -> Result<Value> {
    let chat = app.store.get("chat", chat_id)?;
    let project = app.store.get("project", text(&chat, "projectId"))?;
    let root = PathBuf::from(text(&project, "path"));
    let after = crate::workspace::read(&root, path)?;
    let (before, existed) = match app.store.checkpoint_get(chat_id, path) {
        Ok((before, _)) => {
            let existed = before.is_some();
            (before.unwrap_or_default(), existed)
        }
        Err(_) => {
            let repo = git2::Repository::open(&root)?;
            let tree = repo.head()?.peel_to_tree()?;
            match tree.get_path(Path::new(path)) {
                Ok(entry) => (
                    String::from_utf8(repo.find_blob(entry.id())?.content().to_vec())?,
                    true,
                ),
                Err(_) => (String::new(), false),
            }
        }
    };
    let difference = similar::TextDiff::from_lines(&before, &after);
    let hunks:Vec<Value>=difference.grouped_ops(3).iter().enumerate().map(|(index,group)|{
        let old_start=group.first().unwrap().old_range().start;let old_end=group.last().unwrap().old_range().end;let new_start=group.first().unwrap().new_range().start;let new_end=group.last().unwrap().new_range().end;
        json!({"id":index,"oldStart":old_start,"oldEnd":old_end,"newStart":new_start,"newEnd":new_end,"before":difference.old_slices()[old_start..old_end].concat(),"after":difference.new_slices()[new_start..new_end].concat()})
    }).collect();
    Ok(
        json!({"path":path,"before":before,"after":after,"beforeExists":existed,"hash":hash(&after),"hunks":hunks,"acceptedHunks":app.store.all("review_decision")?.iter().filter(|d|d["chatId"]==chat_id&&d["path"]==path&&d["hash"]==hash(&after)&&d["decision"]=="accept").map(|d|d["hunkId"].clone()).collect::<Vec<_>>(),"comments":app.store.all("review_comment")?.into_iter().filter(|c|c["chatId"]==chat_id&&c["path"]==path).collect::<Vec<_>>()}),
    )
}
pub fn decide_hunk(app: &App, value: &Value) -> Result<Value> {
    let review = review(app, text(value, "chatId"), text(value, "path"))?;
    if review["hash"] != value["hash"] {
        bail!("File changed since review; refresh before accepting/rejecting a hunk");
    }
    let hunk = review["hunks"]
        .as_array()
        .and_then(|a| a.iter().find(|h| h["id"] == value["hunkId"]))
        .context("Hunk is no longer present")?;
    if value["decision"] == "reject" {
        let after = text(&review, "after");
        let lines: Vec<&str> = after.split_inclusive('\n').collect();
        let start = hunk["newStart"].as_u64().unwrap() as usize;
        let end = hunk["newEnd"].as_u64().unwrap() as usize;
        let restored = format!(
            "{}{}{}",
            lines[..start].concat(),
            text(hunk, "before"),
            lines[end..].concat()
        );
        let chat = app.store.get("chat", text(value, "chatId"))?;
        let project = app.store.get("project", text(&chat, "projectId"))?;
        let root = Path::new(text(&project, "path"));
        if restored.is_empty() && review["beforeExists"] == false {
            if crate::workspace::read(root, text(value, "path"))? != after {
                bail!("File changed during review");
            }
            std::fs::remove_file(crate::workspace::resolve(root, text(value, "path"), false)?)?;
            app.store
                .checkpoint_delete(text(value, "chatId"), text(value, "path"))?;
        } else {
            crate::workspace::write(app, "", root, text(value, "path"), &restored, Some(after))?;
            if let Ok((before, _)) = app
                .store
                .checkpoint_get(text(value, "chatId"), text(value, "path"))
            {
                app.store.checkpoint(
                    text(value, "chatId"),
                    text(value, "path"),
                    before.as_deref(),
                    &restored,
                )?;
            }
        }
    } else if value["decision"] != "accept" {
        bail!("Choose accept or reject");
    }
    let item = json!({"id":id(),"chatId":value["chatId"],"path":value["path"],"hunkId":value["hunkId"],"hash":value["hash"],"decision":value["decision"],"at":now()});
    app.store.put("review_decision", &item)?;
    Ok(item)
}
