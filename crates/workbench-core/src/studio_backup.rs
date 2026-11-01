use super::*;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use pbkdf2::pbkdf2_hmac;
use rand::{RngCore, rngs::OsRng};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

const ITERATIONS: u32 = 600_000;
const MAX_TOTAL: usize = 256 * 1024 * 1024;
fn key(passphrase: &str, salt: &[u8], iterations: u32) -> Result<[u8; 32]> {
    if passphrase.chars().count() < 12 {
        bail!("Use a backup passphrase of at least 12 characters");
    }
    if !(100_000..=2_000_000).contains(&iterations) {
        bail!("Unsupported backup key derivation settings");
    }
    let mut key = [0; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);
    Ok(key)
}
fn encrypt(payload: &Value, passphrase: &str) -> Result<Value> {
    let mut salt = [0; 16];
    let mut nonce = [0; 12];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    let key = key(passphrase, &salt, ITERATIONS)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow::anyhow!("Backup cipher initialization failed"))?;
    let bytes = serde_json::to_vec(payload)?;
    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce), bytes.as_slice())
        .map_err(|_| anyhow::anyhow!("Backup encryption failed"))?;
    Ok(
        json!({"format":"workbench-backup-v1","algorithm":"AES-256-GCM/PBKDF2-HMAC-SHA256","iterations":ITERATIONS,"salt":STANDARD.encode(salt),"nonce":STANDARD.encode(nonce),"ciphertext":STANDARD.encode(ciphertext)}),
    )
}
fn decrypt(envelope: &Value, passphrase: &str) -> Result<Value> {
    if envelope["format"] != "workbench-backup-v1" {
        bail!("This is not a supported Workbench backup");
    }
    let salt = STANDARD.decode(text(envelope, "salt"))?;
    let nonce = STANDARD.decode(text(envelope, "nonce"))?;
    if salt.len() != 16 || nonce.len() != 12 {
        bail!("Invalid backup encryption metadata");
    }
    let ciphertext = STANDARD.decode(text(envelope, "ciphertext"))?;
    if ciphertext.len() > MAX_TOTAL * 2 {
        bail!("Backup exceeds restore size limit");
    }
    let key = key(
        passphrase,
        &salt,
        envelope["iterations"]
            .as_u64()
            .context("Missing key derivation settings")?
            .try_into()?,
    )?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow::anyhow!("Backup cipher initialization failed"))?;
    let nonce: [u8; 12] = nonce
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid backup nonce"))?;
    let bytes = cipher
        .decrypt(&Nonce::from(nonce), ciphertext.as_slice())
        .map_err(|_| anyhow::anyhow!("Wrong passphrase or damaged/tampered backup"))?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn skip_root(name: &str) -> bool {
    name.starts_with("workbench.sqlite")
        || [
            "workbench.lock",
            "vault",
            "exports",
            "backups",
            "updates",
            "restore-staging",
            "pending-restore.json",
            "plugin-builds",
            "plugin-staging",
            "catalog-downloads",
            "browser-profiles",
            "browser-artifacts",
        ]
        .contains(&name)
}
fn collect_files(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || !(e.depth() == 1 && skip_root(&e.file_name().to_string_lossy()))
        })
    {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            bail!("Profile backup does not follow symbolic links");
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        total += bytes.len();
        if total > MAX_TOTAL {
            bail!("Profile exceeds the 256 MiB backup limit; export large artifacts separately");
        }
        files.insert(
            entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/"),
            STANDARD.encode(bytes),
        );
    }
    Ok(files)
}
pub(super) async fn create(app: Arc<App>, value: &Value) -> Result<Value> {
    confirm(value)?;
    idle(&app)?;
    let passphrase = text(value, "passphrase").to_string();
    key(&passphrase, &[0; 16], 100_000)?;
    let output = if text(value, "path").is_empty() {
        app.data_dir
            .join("exports")
            .join(format!("Workbench-{}.wbb", id()))
    } else {
        std::path::absolute(text(value, "path"))?
    };
    if output.exists() {
        bail!("Choose a new backup filename; existing files are not overwritten");
    }
    let _guard = app.refresh_lock.lock().await;
    let temporary = app.data_dir.join(format!("backup-{}.sqlite", id()));
    let connection = rusqlite::Connection::open(app.data_dir.join("workbench.sqlite"))?;
    connection.execute("VACUUM INTO ?1", [temporary.to_string_lossy().as_ref()])?;
    let database = std::fs::read(&temporary)?;
    std::fs::remove_file(&temporary)?;
    let files = collect_files(&app.data_dir)?;
    let mut secrets = BTreeMap::new();
    let vault = app.data_dir.join("vault");
    #[cfg(windows)]
    if vault.exists() {
        for file in std::fs::read_dir(vault)? {
            let file = file?;
            if file.path().extension().is_some_and(|s| s == "bin") {
                let name = file
                    .path()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                if name != "runtime" {
                    secrets.insert(name.clone(), crate::auth::get_secret(&app, &name)?);
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = vault;
        for connection in app.store.all("connection")? {
            let id = text(&connection, "id");
            for name in [id.to_string(), format!("{id}-oauth")] {
                if let Ok(secret) = crate::auth::get_secret(&app, &name) {
                    secrets.insert(name, secret);
                }
            }
        }
    }
    let payload = json!({"schema":1,"version":env!("CARGO_PKG_VERSION"),"sourceDataDir":app.data_dir,"database":STANDARD.encode(database),"files":files,"secrets":secrets,"createdAt":now()});
    let envelope = encrypt(&payload, &passphrase)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(&envelope)?;
    std::fs::write(&output, &bytes)?;
    let record = json!({"id":id(),"path":output,"bytes":bytes.len(),"createdAt":now(),"encrypted":true,"credentialCount":secrets.len()});
    app.store.put("studio_backup", &record)?;
    Ok(record)
}
fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty()
        || relative.contains(':')
        || relative.contains('\\')
        || Path::new(relative)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("Backup contains an unsafe path");
    }
    Ok(root.join(relative))
}
fn rebase(value: &mut Value, source: &str, destination: &str) {
    match value {
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix(source)
                && (rest.is_empty() || rest.starts_with(['/', '\\']))
            {
                *s = format!("{destination}{rest}");
            }
        }
        Value::Array(items) => {
            for item in items {
                rebase(item, source, destination);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                rebase(item, source, destination);
            }
        }
        _ => {}
    }
}
pub(super) async fn restore(app: Arc<App>, value: &Value) -> Result<Value> {
    confirm(value)?;
    idle(&app)?;
    let input = std::fs::canonicalize(text(value, "path"))?;
    if std::fs::metadata(&input)?.len() > MAX_TOTAL as u64 * 3 {
        bail!("Backup exceeds restore limit");
    }
    let envelope: Value = serde_json::from_slice(&std::fs::read(input)?)?;
    let payload = decrypt(&envelope, text(value, "passphrase"))?;
    if payload["schema"] != 1 {
        bail!("Unsupported profile schema");
    }
    let pending = text(value, "destination").is_empty();
    let destination = if pending {
        std::fs::canonicalize(&app.data_dir)?
    } else {
        std::path::absolute(text(value, "destination"))?
    };
    let stage = if pending {
        app.data_dir.join("restore-staging").join(id())
    } else {
        destination.clone()
    };
    if !pending && stage.exists() && std::fs::read_dir(&stage)?.next().is_some() {
        bail!(
            "Restore into a new empty profile directory, or leave destination blank to replace this profile after restart"
        );
    }
    std::fs::create_dir_all(&stage)?;
    let database = STANDARD.decode(text(&payload, "database"))?;
    if database.len() > MAX_TOTAL {
        bail!("Backup database is too large");
    }
    std::fs::write(stage.join("workbench.sqlite"), database)?;
    let mut total = 0usize;
    for (relative, encoded) in payload["files"]
        .as_object()
        .context("Backup files are missing")?
    {
        let target = safe_path(&stage, relative)?;
        let bytes = STANDARD.decode(encoded.as_str().context("Invalid backup file encoding")?)?;
        total += bytes.len();
        if total > MAX_TOTAL {
            bail!("Expanded backup exceeds restore limit");
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }
    {
        let db = rusqlite::Connection::open(stage.join("workbench.sqlite"))?;
        let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            bail!("Backup database integrity check failed");
        }
        for table in ["entities", "messages", "events"] {
            let mut query = db.prepare(&format!("SELECT rowid,data FROM {table}"))?;
            let rows = query
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(query);
            for (row, raw) in rows {
                let mut record: Value = serde_json::from_str(&raw)?;
                rebase(
                    &mut record,
                    text(&payload, "sourceDataDir"),
                    &destination.to_string_lossy(),
                );
                db.execute(
                    &format!("UPDATE {table} SET data=?1 WHERE rowid=?2"),
                    rusqlite::params![record.to_string(), row],
                )?;
            }
        }
    }
    let restored = App::new(Some(stage.clone())).await?;
    for (name, secret) in payload["secrets"]
        .as_object()
        .context("Backup credential section missing")?
    {
        if name == "runtime" {
            continue;
        }
        crate::auth::set_secret(
            &restored,
            name,
            secret.as_str().context("Invalid backup credential")?,
        )?;
    }
    drop(restored);
    if pending {
        let marker = app.data_dir.join("pending-restore.json");
        if marker.exists() {
            bail!("A restore is already staged; restart Workbench to finish it");
        }
        std::fs::write(
            marker,
            serde_json::to_vec(&json!({"stage":stage,"createdAt":now()}))?,
        )?;
    }
    Ok(
        json!({"ok":true,"destination":destination,"restartRequired":pending,"message":if pending{"Restore verified and staged. Restart Workbench to apply it; the old profile will be kept as a recovery copy."}else{"Profile restored into the selected directory. Existing profile was preserved."}}),
    )
}
fn copy_directory(source: &Path, target: &Path, skip: &[&str]) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for item in std::fs::read_dir(source)? {
        let item = item?;
        let name = item.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let meta = item.file_type()?;
        if meta.is_symlink() {
            bail!("Profile maintenance refuses symbolic links");
        }
        let to = target.join(name);
        if meta.is_dir() {
            copy_directory(&item.path(), &to, &[])?;
        } else {
            std::fs::copy(item.path(), to)?;
        }
    }
    Ok(())
}
pub(super) fn apply_pending(data_dir: &Path) -> Result<()> {
    let marker = data_dir.join("pending-restore.json");
    if !marker.is_file() {
        return Ok(());
    }
    let root = std::fs::canonicalize(data_dir)?;
    let record: Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
    let stage = std::fs::canonicalize(text(&record, "stage"))?;
    let permitted = std::fs::canonicalize(data_dir.join("restore-staging"))?;
    if !stage.starts_with(&permitted) || stage == permitted {
        bail!("Restore stage escapes its managed directory");
    }
    let recovery = root.with_file_name(format!("Workbench-before-restore-{}", id()));
    copy_directory(
        &root,
        &recovery,
        &[
            "workbench.lock",
            "restore-staging",
            "pending-restore.json",
            "browser-profiles",
            "browser-artifacts",
            "plugin-builds",
            "exports",
            "backups",
            "updates",
        ],
    )?;
    for name in [
        "workbench.sqlite",
        "workbench.sqlite-wal",
        "workbench.sqlite-shm",
        "vault",
        "plugins",
        "plugin-data",
        "attachments",
    ] {
        let path = root.join(name);
        if !path.exists() {
            continue;
        }
        let canonical = std::fs::canonicalize(&path)?;
        if !canonical.starts_with(&root) || canonical == root {
            bail!("Refusing to replace an external profile path");
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    copy_directory(&stage, &root, &["workbench.lock"])?;
    std::fs::remove_file(marker)?;
    std::fs::write(
        root.join("last-restore.json"),
        serde_json::to_vec(&json!({"recovery":recovery,"restoredAt":now()}))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_requires_the_correct_passphrase_and_rejects_tampering() {
        let value = json!({"secret":"fixture-only","files":[1,2]});
        let mut envelope = encrypt(&value, "a long fixture passphrase").unwrap();
        assert_eq!(
            decrypt(&envelope, "a long fixture passphrase").unwrap(),
            value
        );
        assert!(decrypt(&envelope, "another long fixture passphrase").is_err());
        envelope["ciphertext"] = json!(STANDARD.encode(b"tampered"));
        assert!(decrypt(&envelope, "a long fixture passphrase").is_err());
    }
    #[test]
    fn restore_paths_cannot_escape() {
        let root = Path::new("profile");
        for bad in ["../outside", "/outside", "C:/outside", "a\\b", ""] {
            assert!(safe_path(root, bad).is_err());
        }
        assert!(safe_path(root, "plugins/example/plugin.json").is_ok());
    }
}
