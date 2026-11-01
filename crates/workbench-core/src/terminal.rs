use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Session {
    owner: std::path::PathBuf,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    output: Arc<Mutex<(u64, String)>>,
    cancel: CancellationToken,
}
static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<Session>>>> = OnceLock::new();
fn sessions() -> &'static Mutex<HashMap<String, Arc<Session>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn session(app: &App, identity: &str) -> Result<Arc<Session>> {
    sessions()
        .lock()
        .get(identity)
        .filter(|s| s.owner == app.data_dir)
        .cloned()
        .context("Terminal session is closed or belongs to another profile")
}
pub fn state(app: &App) -> Result<Vec<Value>> {
    let live = sessions().lock();
    Ok(app
        .store
        .all("terminal")?
        .into_iter()
        .filter(|t| {
            live.get(text(t, "id"))
                .is_some_and(|s| s.owner == app.data_dir)
        })
        .collect())
}

pub fn start(app: Arc<App>, value: Value) -> Result<Value> {
    if value["confirmed"] != true {
        bail!("Confirm native terminal execution with your Windows user authority");
    }
    let project = app.store.get("project", text(&value, "projectId"))?;
    if sessions()
        .lock()
        .values()
        .filter(|s| s.owner == app.data_dir)
        .count()
        >= 8
    {
        bail!("Close an existing terminal before opening another");
    }
    let program = if text(&value, "program").is_empty() {
        if cfg!(windows) {
            "powershell.exe"
        } else {
            "/bin/sh"
        }
    } else {
        text(&value, "program")
    };
    let mut command = CommandBuilder::new(program);
    command.cwd(text(&project, "path"));
    command.env_clear();
    for key in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "PATHEXT",
        "COMSPEC",
    ] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    command.env("TERM", "xterm-256color");
    if let Some(args) = value["args"].as_array() {
        for arg in args {
            command.arg(arg.as_str().context("Terminal arguments must be strings")?);
        }
    } else if cfg!(windows) && program == "powershell.exe" {
        command.arg("-NoLogo");
        command.arg("-NoProfile");
    }
    let pair = portable_pty::native_pty_system().openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    #[cfg(windows)]
    let job = crate::runtime::CommandJob::new()?;
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    #[cfg(windows)]
    if let Err(error) = job.attach_handle(
        child
            .as_raw_handle()
            .context("Terminal process handle unavailable")?,
    ) {
        let _ = child.kill();
        return Err(error);
    }
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let identity = id();
    let cancel = CancellationToken::new();
    let output = Arc::new(Mutex::new((0, String::new())));
    let session = Arc::new(Session {
        owner: app.data_dir.clone(),
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        output: output.clone(),
        cancel: cancel.clone(),
    });
    sessions().lock().insert(identity.clone(), session);
    app.runs
        .lock()
        .insert(format!("terminal:{identity}"), cancel.clone());
    let record = json!({"id":identity,"projectId":project["id"],"program":program,"status":"running","createdAt":now()});
    app.store.put("terminal", &record)?;
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut bytes = [0u8; 8192];
        while let Ok(size) = reader.read(&mut bytes) {
            if size == 0 {
                break;
            }
            let mut buffer = output.lock();
            buffer.1.push_str(&String::from_utf8_lossy(&bytes[..size]));
            if buffer.1.len() > 512 * 1024 {
                let mut cut = buffer.1.len() - 256 * 1024;
                while !buffer.1.is_char_boundary(cut) {
                    cut += 1;
                }
                buffer.1.drain(..cut);
                buffer.0 += cut as u64;
            }
        }
    });
    let weak = Arc::downgrade(&app);
    let response = record.clone();
    let task_id = identity.clone();
    std::thread::spawn(move || {
        #[cfg(windows)]
        let _job = job;
        let status = loop {
            if cancel.is_cancelled() || weak.strong_count() == 0 {
                let _ = child.kill();
                break "canceled";
            }
            match child.try_wait() {
                Ok(Some(_)) => break "exited",
                Err(_) => break "failed",
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if let Some(app) = weak.upgrade() {
            let mut final_record = record;
            final_record["status"] = json!(status);
            let _ = app.store.put("terminal", &final_record);
            app.runs.lock().remove(&format!("terminal:{task_id}"));
        }
    });
    Ok(response)
}
pub fn read(app: &App, identity: &str, cursor: u64) -> Result<Value> {
    let session = session(app, identity)?;
    let output = session.output.lock();
    let mut from = cursor.saturating_sub(output.0).min(output.1.len() as u64) as usize;
    while !output.1.is_char_boundary(from) {
        from += 1;
    }
    Ok(
        json!({"text":output.1[from..],"cursor":output.0+output.1.len()as u64,"truncated":cursor<output.0,"status":app.store.get("terminal",identity).ok().map(|v|v["status"].clone())}),
    )
}
pub fn input(app: &App, identity: &str, value: &Value) -> Result<Value> {
    let session = session(app, identity)?;
    let content = text(value, "text");
    if content.len() > 64000 {
        bail!("Terminal input exceeds 64 KiB");
    }
    let mut writer = session.writer.lock();
    writer.write_all(content.as_bytes())?;
    writer.flush()?;
    Ok(json!({"ok":true}))
}
pub fn resize(app: &App, identity: &str, value: &Value) -> Result<Value> {
    let session = session(app, identity)?;
    session.master.lock().resize(PtySize {
        rows: value["rows"].as_u64().unwrap_or(30).clamp(2, 200) as u16,
        cols: value["cols"].as_u64().unwrap_or(100).clamp(10, 400) as u16,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    Ok(json!({"ok":true}))
}
pub fn stop(app: &App, identity: &str) -> Result<Value> {
    let session = session(app, identity)?;
    session.cancel.cancel();
    sessions().lock().remove(identity);
    Ok(json!({"ok":true}))
}
