use super::*;
use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

const PUBLIC_KEY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.github/workbench-update-key.pub"
));

fn verify(envelope: &Value, key: &str) -> Result<Value> {
    let bytes = STANDARD.decode(text(envelope, "payload"))?;
    if bytes.len() > 128 * 1024 {
        bail!("Update manifest is too large");
    }
    let key: [u8; 32] = STANDARD
        .decode(key.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Update public key must contain 32 bytes"))?;
    let signature = Signature::from_slice(&STANDARD.decode(text(envelope, "signature"))?)?;
    VerifyingKey::from_bytes(&key)?
        .verify_strict(&bytes, &signature)
        .context("Update signature does not match the trusted publisher key")?;
    let payload: Value = serde_json::from_slice(&bytes)?;
    if payload["platform"] != "windows-x86_64" {
        bail!("This update targets a different platform");
    }
    version(text(&payload, "version"))?;
    let hash = text(&payload, "sha256");
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Signed update has an invalid SHA-256 digest");
    }
    let address = url::Url::parse(text(&payload, "url"))?;
    if address.scheme() != "https" || !address.username().is_empty() || address.password().is_some()
    {
        bail!("Signed update needs a credential-free HTTPS URL");
    }
    Ok(payload)
}
fn version(value: &str) -> Result<(u64, u64, u64)> {
    let parts = value
        .split('.')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if parts.len() != 3 {
        bail!("Update version must use major.minor.patch");
    }
    Ok((parts[0], parts[1], parts[2]))
}
async fn manifest_url(app: &App, channel: &str) -> Result<String> {
    let response = app
        .client
        .get("https://api.github.com/repos/RobertIonutF/Workbench/releases?per_page=30")
        .header("User-Agent", "Workbench")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Release lookup returned {}", response.status());
    }
    let releases: Value = response.json().await?;
    for release in releases.as_array().context("Invalid release catalog")? {
        if release["draft"] == true || (channel == "stable" && release["prerelease"] == true) {
            continue;
        }
        if let Some(asset) = release["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["name"] == "workbench-update.json"))
        {
            return Ok(text(asset, "browser_download_url").into());
        }
    }
    bail!("No signed update manifest is published for this channel yet")
}
pub(super) async fn check(app: &App, value: &Value) -> Result<Value> {
    let channel = if text(value, "channel") == "stable" {
        "stable"
    } else {
        "preview"
    };
    let custom = text(value, "publicKey");
    if !custom.is_empty() && custom.trim() != PUBLIC_KEY.trim() {
        confirm(value)?;
    }
    let key = if custom.is_empty() {
        PUBLIC_KEY.trim()
    } else {
        custom.trim()
    };
    let address = if text(value, "manifestUrl").is_empty() {
        manifest_url(app, channel).await?
    } else {
        text(value, "manifestUrl").into()
    };
    let bytes = super::plugin::download(app, &address, 256 * 1024).await?;
    let envelope: Value = serde_json::from_slice(&bytes)?;
    let payload = verify(&envelope, key)?;
    if payload["channel"].as_str().unwrap_or("preview") != channel {
        bail!("Signed manifest belongs to a different update channel");
    }
    let entry = json!({"id":id(),"version":payload["version"],"releaseNotes":payload["releaseNotes"],"channel":channel,"available":version(text(&payload,"version"))?>version(env!("CARGO_PKG_VERSION"))?,"verified":true,"manifestUrl":address,"publicKey":key,"envelope":envelope,"createdAt":now()});
    app.store.put("studio_update", &entry)?;
    Ok(entry)
}
pub(super) async fn install(app: &App, value: &Value) -> Result<Value> {
    confirm(value)?;
    idle(app)?;
    let entry = app.store.get("studio_update", text(value, "id"))?;
    let payload = verify(&entry["envelope"], text(&entry, "publicKey"))?;
    if version(text(&payload, "version"))? <= version(env!("CARGO_PKG_VERSION"))? {
        bail!("This update is not newer than the running version");
    }
    let bytes = super::plugin::download(app, text(&payload, "url"), 200 * 1024 * 1024).await?;
    if format!("{:x}", Sha256::digest(&bytes)) != text(&payload, "sha256").to_ascii_lowercase() {
        bail!("Update download failed its signed digest check");
    }
    let folder = app.data_dir.join("updates");
    std::fs::create_dir_all(&folder)?;
    let installer = folder.join(format!("Workbench-{}-setup.exe", text(&payload, "version")));
    std::fs::write(&installer, bytes)?;
    #[cfg(windows)]
    {
        let current = std::env::current_exe()?;
        let helper = if current
            .file_stem()
            .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("workbench-cli"))
        {
            current.clone()
        } else {
            current
                .parent()
                .context("Application directory unavailable")?
                .join("workbench-cli.exe")
        };
        if !helper.is_file() {
            bail!(
                "The bundled workbench-cli.exe update helper is missing. Download the full installer or portable package."
            );
        }
        let log = std::fs::File::create(folder.join("installation.log"))?;
        let mut cmd = std::process::Command::new(helper);
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000)
            .arg("apply-update")
            .arg("--wait-pid")
            .arg(std::process::id().to_string())
            .arg("--installer")
            .arg(&installer)
            .arg("--sha256")
            .arg(text(&payload, "sha256"))
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        Ok(
            json!({"ok":true,"restartRequired":true,"path":installer,"message":"The signed update has been verified. Close Workbench; the installer will run after this process exits."}),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = installer;
        bail!("Automatic installation is available on Windows only");
    }
}

pub(super) async fn apply_after_exit(pid: u32, path: &std::path::Path, hash: &str) -> Result<()> {
    if pid == std::process::id() {
        bail!("The update helper cannot wait for itself");
    }
    let path = std::fs::canonicalize(path)?;
    let hash = hash.to_ascii_lowercase();
    if path
        .extension()
        .is_none_or(|s| !s.to_string_lossy().eq_ignore_ascii_case("exe"))
        || hash.len() != 64
        || !hash.chars().all(|c| c.is_ascii_hexdigit())
    {
        bail!("Invalid installer path or SHA-256 digest");
    }
    let bytes = std::fs::read(&path)?;
    if format!("{:x}", Sha256::digest(&bytes)) != hash {
        bail!("Installer digest changed before the update helper started");
    }
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move||->Result<()>{
            use std::io::Read;use std::os::windows::{fs::OpenOptionsExt,process::CommandExt};
            use windows_sys::Win32::{Foundation::{CloseHandle,GetLastError,ERROR_INVALID_PARAMETER,WAIT_OBJECT_0},System::Threading::{OpenProcess,WaitForSingleObject}};
            let handle=unsafe{OpenProcess(0x00100000,0,pid)};
            if handle.is_null(){if unsafe{GetLastError()}!=ERROR_INVALID_PARAMETER{bail!("Could not wait for the running Workbench process");}}
            else{let status=unsafe{WaitForSingleObject(handle,600_000)};unsafe{CloseHandle(handle)};if status!=WAIT_OBJECT_0{bail!("Workbench did not close within ten minutes; run the verified installer manually");}}
            let mut file=std::fs::OpenOptions::new().read(true).share_mode(1).open(&path)?;
            let mut bytes=vec![];file.read_to_end(&mut bytes)?;
            if format!("{:x}",Sha256::digest(&bytes))!=hash{bail!("Installer digest changed while waiting for Workbench to close");}
            let mut child=std::process::Command::new(&path).arg("/S").creation_flags(0x08000000).stdin(std::process::Stdio::null()).spawn()?;
            drop(file);let status=child.wait()?;if !status.success(){bail!("Update installer returned {status}");}
            Ok(())
        }).await??;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (path, hash);
        bail!("Automatic installation is available on Windows only");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[test]
    fn signed_manifest_rejects_tampering_and_wrong_publishers() {
        let key = SigningKey::from_bytes(&[42; 32]);
        let bytes=br#"{"version":"9.0.0","platform":"windows-x86_64","url":"https://example.com/update.exe","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
        let public = STANDARD.encode(key.verifying_key().as_bytes());
        let mut envelope = json!({"payload":STANDARD.encode(bytes),"signature":STANDARD.encode(key.sign(bytes).to_bytes())});
        assert_eq!(verify(&envelope, &public).unwrap()["version"], "9.0.0");
        envelope["payload"] = json!(STANDARD.encode(b"tampered"));
        assert!(verify(&envelope, &public).is_err());
        assert!(
            verify(
                &envelope,
                &STANDARD.encode(SigningKey::from_bytes(&[43; 32]).verifying_key().as_bytes())
            )
            .is_err()
        );
    }
}
