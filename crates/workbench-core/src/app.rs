use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

pub struct App {
    _data_lock: std::fs::File,
    pub store: crate::store::Store,
    pub data_dir: PathBuf,
    pub client: reqwest::Client,
    pub runs: Mutex<HashMap<String, CancellationToken>>,
    pub approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    pub oauth: Mutex<HashMap<String, Value>>,
    pub token: String,
    pub refresh_lock: tokio::sync::Mutex<()>,
    pub services_started: std::sync::atomic::AtomicBool,
    pub scheduler_lock: tokio::sync::Mutex<()>,
    pub run_start_lock: Mutex<()>,
    pub shutting_down: std::sync::atomic::AtomicBool,
}
impl App {
    pub async fn new(directory: Option<PathBuf>) -> Result<Arc<Self>> {
        let data_dir = directory
            .or_else(|| std::env::var_os("WORKBENCH_DATA_DIR").map(PathBuf::from))
            .unwrap_or_else(|| {
                dirs::data_local_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("Workbench")
            });
        std::fs::create_dir_all(&data_dir)?;
        let data_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join("workbench.lock"))?;
        data_lock.try_lock().context("This Workbench data directory is already in use. Reuse the running app or choose a separate --data-dir for a second runtime.")?;
        crate::studio::apply_pending_restore(&data_dir)?;
        let app = Arc::new(Self {
            _data_lock: data_lock,
            store: crate::store::Store::open(&data_dir.join("workbench.sqlite"))?,
            data_dir,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()?,
            runs: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            oauth: Mutex::new(HashMap::new()),
            token: format!("{}{}", id(), id()),
            refresh_lock: tokio::sync::Mutex::new(()),
            services_started: std::sync::atomic::AtomicBool::new(false),
            scheduler_lock: tokio::sync::Mutex::new(()),
            run_start_lock: Mutex::new(()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
        });
        for mut run in app.store.all("run")? {
            if run["status"] == "running" || run["status"] == "paused" {
                run["status"] = json!("interrupted");
                run["error"] = json!(
                    "Workbench stopped before this run finished. Review tool results before resuming."
                );
                app.store.put("run", &run)?;
                app.reconcile_tools(text(&run,"chatId"),text(&run,"id"),"Workbench stopped before the tool result was confirmed. Its external effects are uncertain. Inspect current files/state before deciding whether to retry.")?;
            }
        }
        for mut approval in app.store.all("approval")? {
            if approval["status"] == "pending" {
                approval["status"] = json!("denied");
                app.store.put("approval", &approval)?;
            }
        }
        if app.store.get("settings", "settings").is_err() {
            app.store.put("settings",&json!({"id":"settings","theme":"dark","maxIterations":20,"commandTimeoutSecs":120,"autoActivateSkills":false}))?;
        }
        if app.store.get("meta", "host").is_err() {
            app.store.put(
                "meta",
                &json!({"id":"host","hostId":format!("urn:uuid:{}",id())}),
            )?;
        }
        crate::features::recover(&app)?;
        Ok(app)
    }
    pub fn settings(&self) -> Value {
        self.store.get("settings", "settings").unwrap_or(json!({}))
    }
    pub fn begin_shutdown(&self) {
        let _guard = self.run_start_lock.lock();
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for cancel in self.runs.lock().values() {
            cancel.cancel();
        }
    }
    pub fn cancel_plugin_runs(&self, plugin_id: &str) -> Result<()> {
        let chats: Vec<Value> = self
            .store
            .all("chat")?
            .into_iter()
            .filter(|c| {
                c["pluginIds"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|p| p == plugin_id))
            })
            .collect();
        for run in self.store.all("run")? {
            if run["status"] == "running"
                && chats.iter().any(|c| c["id"] == run["chatId"])
                && let Some(cancel) = self.runs.lock().get(text(&run, "id"))
            {
                cancel.cancel();
            }
        }
        Ok(())
    }
    pub fn state(&self) -> Result<Value> {
        let mut value = json!({"connections":self.store.all("connection")?,"projects":self.store.all("project")?,"chats":self.store.all("chat")?,"plugins":self.store.all("plugin")?,"runs":self.store.all("run")?,"approvals":self.store.all("approval")?,"settings":self.settings(),"version":env!("CARGO_PKG_VERSION")});
        value["terminals"] = json!(crate::terminal::state(self)?);
        for kind in [
            "profile",
            "usage",
            "memory",
            "hook",
            "schedule",
            "notification",
            "comparison",
            "worktree",
            "team",
            "attachment",
        ] {
            value[match kind {
                "usage" => "usage".to_owned(),
                "memory" => "memories".to_owned(),
                _ => format!("{kind}s"),
            }] = json!(
                self.store
                    .all(kind)?
                    .into_iter()
                    .map(|mut item| {
                        if kind == "attachment"
                            && let Some(map) = item.as_object_mut()
                        {
                            map.remove("dataBase64");
                            map.remove("content");
                        }
                        item
                    })
                    .collect::<Vec<_>>()
            );
        }
        value["queue"] = json!({"turns":self.store.all("queued_turn")?.into_iter().filter(|m|m["status"]=="queued"||m["status"]=="starting").collect::<Vec<_>>(),"steering":self.store.all("steering")?.into_iter().filter(|m|m["status"]=="queued").collect::<Vec<_>>()});
        Ok(value)
    }
    pub fn connection(&self, mut v: Value) -> Result<Value> {
        let connection_id = if text(&v, "id").is_empty() {
            id()
        } else {
            text(&v, "id").to_owned()
        };
        let old = self
            .store
            .get("connection", &connection_id)
            .unwrap_or(json!({}));
        if self.store.all("chat")?.iter().any(|c| {
            c["connectionId"] == connection_id
                && self
                    .store
                    .all("run")
                    .unwrap_or_default()
                    .iter()
                    .any(|r| r["chatId"] == c["id"] && crate::tasks::active(r))
        }) {
            bail!("Finish or cancel active runs before changing their model connection");
        }
        let endpoint =
            url::Url::parse(text(&v, "baseUrl")).context("Enter a complete API base URL")?;
        if !["http", "https"].contains(&endpoint.scheme())
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            bail!("API URL must be HTTP(S), without userinfo, query or fragment")
        }
        if endpoint.scheme() == "http"
            && !matches!(
                endpoint.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            )
        {
            bail!("Use HTTPS for non-loopback API endpoints")
        }
        if !matches!(
            text(&v, "protocol"),
            "openai_chat" | "openai_responses" | "anthropic" | "gemini"
        ) {
            bail!("Unsupported API protocol")
        }
        let new_secret = text(&v, "apiKey").to_owned();
        if old["authKind"] == "openai_oauth" {
            if v["authKind"] != "openai_oauth"
                || v["baseUrl"] != "https://api.openai.com/v1"
                || v["protocol"] != "openai_responses"
                || !new_secret.is_empty()
            {
                bail!(
                    "Keep the ChatGPT OAuth route unchanged. Create a separate API connection for another endpoint or billing route."
                );
            }
            for key in ["clientId", "subject"] {
                v[key] = old[key].clone();
            }
        } else if v["authKind"] == "openai_oauth" {
            bail!("Use Continue with ChatGPT to create an authenticated OAuth connection");
        }
        if !text(&old, "baseUrl").is_empty()
            && text(&old, "baseUrl") != text(&v, "baseUrl")
            && new_secret.is_empty()
            && old["secretPresent"] == true
        {
            bail!(
                "Re-enter the credential when changing an endpoint; credentials are bound to their destination"
            )
        }
        let mut secret_present = old["secretPresent"].as_bool().unwrap_or(false);
        if v["removeSecret"] == true {
            crate::auth::delete_secret(self, &connection_id)?;
            secret_present = false;
        }
        if !new_secret.is_empty() {
            crate::auth::set_secret(self, &connection_id, &json!({"workbenchCredential":"api-key-v2","destination":v["baseUrl"],"protocol":v["protocol"],"apiKey":new_secret}).to_string())?;
            secret_present = true;
        }
        if let Some(map) = v.as_object_mut() {
            map.remove("apiKey");
            map.remove("removeSecret");
        }
        v["id"] = json!(connection_id);
        v["secretPresent"] = json!(secret_present);
        v["createdAt"] = old.get("createdAt").cloned().unwrap_or(json!(now()));
        if v["contextWindow"].as_u64().is_none() {
            v["contextWindow"] = json!(32768);
        }
        if v["tools"].as_bool().is_none() {
            v["tools"] = json!(false);
        }
        if v["authKind"].is_null() {
            v["authKind"] = json!(if secret_present { "api_key" } else { "none" });
        }
        if !matches!(text(&v, "authKind"), "none" | "api_key" | "openai_oauth") {
            bail!("Unsupported authentication method");
        }
        v["contextWindow"] = json!(
            v["contextWindow"]
                .as_u64()
                .unwrap_or(32768)
                .clamp(512, 10_000_000)
        );
        self.store.put("connection", &v)?;
        Ok(v)
    }
    pub fn project(&self, v: Value) -> Result<Value> {
        let root =
            std::fs::canonicalize(text(&v, "path")).context("Project directory does not exist")?;
        if !root.is_dir() {
            bail!("Select a directory")
        };
        if let Some(existing) = self
            .store
            .all("project")?
            .into_iter()
            .find(|p| std::path::Path::new(text(p, "path")) == root)
        {
            return Ok(existing);
        }
        let value = json!({"id":id(),"name":if text(&v,"name").is_empty(){root.file_name().map(|s|s.to_string_lossy().to_string()).unwrap_or("Project".into())}else{text(&v,"name").to_owned()},"path":root.to_string_lossy(),"createdAt":now()});
        self.store.put("project", &value)?;
        Ok(value)
    }
    pub fn chat(&self, mut v: Value) -> Result<Value> {
        if !text(&v, "profileId").is_empty() {
            let profile = self.store.get("profile", text(&v, "profileId"))?;
            v["connectionId"] = profile["connectionId"].clone();
            v["modelId"] = profile["modelId"].clone();
        }
        if text(&v, "connectionId").is_empty() {
            bail!("Choose a model connection first")
        };
        let connection = self.store.get("connection", text(&v, "connectionId"))?;
        if text(&v, "modelId").is_empty() {
            v["modelId"] = connection["modelId"].clone();
        }
        if text(&v, "modelId").is_empty() {
            bail!("Enter or discover a model ID")
        }
        if !text(&v, "projectId").is_empty() {
            self.store.get("project", text(&v, "projectId"))?;
        }
        if text(&v, "id").is_empty() {
            v["id"] = json!(id());
            v["createdAt"] = json!(now());
        }
        if text(&v, "title").is_empty() {
            v["title"] = json!("New chat");
        }
        for key in ["pluginIds", "skillIds"] {
            if !v[key].is_array() {
                v[key] = json!([]);
            }
        }
        if !matches!(
            text(&v, "executionMode"),
            "ask" | "read_only" | "trusted_native" | "docker"
        ) {
            v["executionMode"] = json!("ask");
        }
        v["updatedAt"] = json!(now());
        self.store.put("chat", &v)?;
        Ok(v)
    }
    pub fn add_message(
        &self,
        chat: &str,
        role: &str,
        content: &str,
        extra: Value,
    ) -> Result<Value> {
        let mut value =
            json!({"id":id(),"chatId":chat,"role":role,"content":content,"createdAt":now()});
        if let Some(map) = extra.as_object() {
            for (k, v) in map {
                value[k] = v.clone();
            }
        }
        self.store.message(&value)?;
        Ok(value)
    }
    pub fn reconcile_tools(&self, chat: &str, run: &str, reason: &str) -> Result<()> {
        let mut pending: HashMap<String, (String, Value, Value)> = HashMap::new();
        for message in self.store.messages(chat)? {
            if let Some(calls) = message["canonical"]["tool_calls"].as_array() {
                for call in calls {
                    pending.insert(
                        text(call, "id").into(),
                        (
                            text(call, "name").into(),
                            message["connectionId"].clone(),
                            message["modelId"].clone(),
                        ),
                    );
                }
            }
            if let Some(call) = message["canonical"]["tool_call_id"].as_str() {
                pending.remove(call);
            }
        }
        for (call, (name, connection, model)) in pending {
            let content=json!({"error":reason,"status":"interrupted","effects":"unknown","automaticallyRetried":false}).to_string();
            let mut canonical = crate::providers::Message::new("tool", content.clone());
            canonical.tool_call_id = Some(call.clone());
            self.add_message(chat,"tool",&content,json!({"toolName":name,"canonical":canonical,"connectionId":connection,"modelId":model}))?;
            self.store.event(
                run,
                chat,
                "tool_result",
                json!({"id":call,"name":name,"result":{"error":reason,"effects":"unknown"}}),
            )?;
        }
        Ok(())
    }
    pub async fn approve(
        &self,
        run: &str,
        chat: &str,
        title: &str,
        details: Value,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let approval_id = id();
        let approval = json!({"id":approval_id,"runId":run,"chatId":chat,"title":title,"details":details,"status":"pending","createdAt":now()});
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().insert(approval_id.clone(), tx);
        self.store.put("approval", &approval)?;
        self.store.event(run, chat, "approval", approval.clone())?;
        let accepted = tokio::select! {v=rx=>v.unwrap_or(false),_=cancel.cancelled()=>false,_=tokio::time::sleep(std::time::Duration::from_secs(600))=>false};
        self.approvals.lock().remove(&approval_id);
        let mut final_value = approval;
        final_value["status"] = json!(if accepted { "allowed" } else { "denied" });
        self.store.put("approval", &final_value)?;
        if !accepted {
            bail!("Action was denied, canceled or expired")
        };
        Ok(())
    }
}
