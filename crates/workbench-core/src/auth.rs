use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};

fn secret_path(app: &App, key: &str) -> Result<std::path::PathBuf> {
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("Invalid credential reference")
    };
    let dir = app.data_dir.join("vault");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{key}.bin")))
}
pub fn runtime_info(directory: Option<std::path::PathBuf>) -> Result<Value> {
    let directory = directory
        .or_else(|| std::env::var_os("WORKBENCH_DATA_DIR").map(std::path::PathBuf::from))
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| ".".into())
                .join("Workbench")
        });
    #[cfg(windows)]
    let raw = String::from_utf8(crypt(
        &std::fs::read(directory.join("vault/runtime.bin"))?,
        false,
    )?)?;
    #[cfg(not(windows))]
    let raw = keyring::Entry::new("Workbench", "runtime")?.get_password()?;
    let value: Value = serde_json::from_str(&raw)?;
    let url = url::Url::parse(text(&value, "baseUrl"))?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        bail!("Invalid local runtime discovery record");
    }
    Ok(value)
}
#[cfg(windows)]
fn crypt(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into()?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // DPAPI binds encrypted data to this Windows user. Never expose plaintext to logs or the UI.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    };
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut _);
    };
    Ok(result)
}
pub fn set_secret(app: &App, key: &str, value: &str) -> Result<()> {
    #[cfg(windows)]
    {
        let path = secret_path(app, key)?;
        let tmp = path.with_extension(format!("{}.tmp", id()));
        std::fs::write(&tmp, crypt(value.as_bytes(), true)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        keyring::Entry::new("Workbench", key)?.set_password(value)?;
        let _ = app;
        Ok(())
    }
}
pub fn get_secret(app: &App, key: &str) -> Result<String> {
    #[cfg(windows)]
    {
        Ok(String::from_utf8(crypt(
            &std::fs::read(secret_path(app, key)?)
                .context("Credential is missing; reconnect this account")?,
            false,
        )?)?)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Ok(keyring::Entry::new("Workbench", key)?.get_password()?)
    }
}
pub fn delete_secret(app: &App, key: &str) -> Result<()> {
    #[cfg(windows)]
    {
        let path = secret_path(app, key)?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        let _ = keyring::Entry::new("Workbench", key)?.delete_credential();
        Ok(())
    }
}
pub async fn access_token(app: &App, connection: &Value) -> Result<String> {
    let _guard = app.refresh_lock.lock().await;
    let key = format!("{}-oauth", text(connection, "id"));
    let mut credential: Value = serde_json::from_str(&get_secret(app, &key)?)?;
    if credential["expiresAt"].as_i64().unwrap_or(0) > chrono::Utc::now().timestamp() + 60 {
        return Ok(text(&credential, "access_token").into());
    }
    let response = app
        .client
        .post("https://auth.openai.com/api/accounts/oauth/token")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", text(&credential, "refresh_token")),
            ("client_id", text(&credential, "client_id")),
            ("resource", "https://api.openai.com/v1"),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("ChatGPT authorization expired or was revoked; sign in again")
    };
    let updated: Value = response.json().await?;
    for k in ["access_token", "refresh_token", "id_token", "scope"] {
        if updated[k].is_string() {
            credential[k] = updated[k].clone();
        }
    }
    credential["expiresAt"] =
        json!(chrono::Utc::now().timestamp() + updated["expires_in"].as_i64().unwrap_or(3600));
    set_secret(app, &key, &credential.to_string())?;
    Ok(text(&credential, "access_token").into())
}
pub async fn start(app: Arc<App>) -> Result<Value> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}/auth/callback");
    let state = id();
    let nonce = id();
    let verifier = format!("{}{}", id(), id()).replace('-', "");
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let login_id = id();
    let host = app.store.get("meta", "host")?;
    let mut url = url::Url::parse("https://auth.openai.com/api/accounts/authorize")?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", "dynamic_agent_client"),
        ("agent_name_hint", "Workbench"),
        ("ext_agent_host_id", text(&host, "hostId")),
        ("response_type", "code"),
        ("redirect_uri", &redirect),
        (
            "scope",
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
        ),
        ("resource", "https://api.openai.com/v1"),
        ("state", &state),
        ("nonce", &nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
    ]);
    app.oauth
        .lock()
        .insert(login_id.clone(), json!({"status":"pending"}));
    let result = json!({"url":url.as_str(),"loginId":login_id});
    tokio::spawn(async move {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sender = Arc::new(parking_lot::Mutex::new(Some(tx)));
        let router=axum::Router::new().route("/auth/callback",axum::routing::get(move|axum::extract::Query(query):axum::extract::Query<HashMap<String,String>>|{let sender=sender.clone();async move{if let Some(tx)=sender.lock().take(){let _=tx.send(query);}axum::response::Html("<!doctype html><title>Workbench</title><body style='font:18px system-ui;background:#111315;color:#eee;padding:60px'>Return to Workbench to see the connection result. You can close this tab.</body>")}}));
        let stop = tokio_util::sync::CancellationToken::new();
        let stop_server = stop.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(stop_server.cancelled_owned())
                .await;
        });
        let outcome=async{
            let query=tokio::time::timeout(std::time::Duration::from_secs(300),rx).await.context("Sign-in timed out")??;
            if query.get("state")!=Some(&state){bail!("Sign-in state validation failed")};if query.contains_key("error"){bail!("Sign-in was declined")};
            let code=query.get("code").context("No authorization code received")?;let client=query.get("client_id").context("No issued OAuth client ID returned")?;
            if client=="dynamic_agent_client"||client.is_empty(){bail!("Invalid issued client ID")}
            let response=app.client.post("https://auth.openai.com/api/accounts/oauth/token").form(&[("grant_type","authorization_code"),("client_id",client.as_str()),("code",code.as_str()),("code_verifier",&verifier),("redirect_uri",&redirect),("resource","https://api.openai.com/v1")]).send().await?;
            if !response.status().is_success(){bail!("OAuth code exchange failed; start a fresh sign-in")};let mut tokens:Value=response.json().await?;
            if !text(&tokens,"scope").split_whitespace().any(|s|s=="chatgpt.tokens.use.direct"){bail!("ChatGPT plan usage was not authorized")}
            let discovery:Value=app.client.get("https://auth.openai.com/.well-known/openid-configuration").send().await?.error_for_status()?.json().await?;
            let jwks_url=url::Url::parse(text(&discovery,"jwks_uri"))?;if jwks_url.scheme()!="https"||jwks_url.host_str()!=Some("auth.openai.com"){bail!("Unexpected identity key endpoint")}
            let jwks:jsonwebtoken::jwk::JwkSet=app.client.get(jwks_url).send().await?.error_for_status()?.json().await?;
            let encoded=text(&tokens,"id_token");let header=jsonwebtoken::decode_header(encoded)?;if !matches!(header.alg,jsonwebtoken::Algorithm::RS256|jsonwebtoken::Algorithm::ES256){bail!("Unsupported identity signature algorithm")};let key=jwks.find(header.kid.as_deref().context("Identity token lacks key ID")?).context("Identity signing key not found")?;
            let mut validation=jsonwebtoken::Validation::new(header.alg);validation.set_audience(&[client]);validation.set_issuer(&["https://auth.openai.com"]);validation.set_required_spec_claims(&["exp","iss","aud","sub"]);
            let claims=jsonwebtoken::decode::<Value>(encoded,&jsonwebtoken::DecodingKey::from_jwk(key)?,&validation)?.claims;
            if text(&claims,"nonce")!=nonce{bail!("Identity nonce validation failed")}
            let connection_id=id();tokens["client_id"]=json!(client);tokens["expiresAt"]=json!(chrono::Utc::now().timestamp()+tokens["expires_in"].as_i64().unwrap_or(3600));set_secret(&app,&format!("{connection_id}-oauth"),&tokens.to_string())?;
            let connection=json!({"id":connection_id,"label":format!("ChatGPT · {}",claims["email"].as_str().unwrap_or("account")),"preset":"openai","protocol":"openai_responses","baseUrl":"https://api.openai.com/v1","modelId":"","contextWindow":32768,"tools":true,"secretPresent":true,"authKind":"openai_oauth","subject":claims["sub"],"clientId":client,"createdAt":now()});app.store.put("connection",&connection)?;Ok::<_,anyhow::Error>(())
        }.await;
        stop.cancel();
        let _ = task.await;
        app.oauth.lock().insert(
            login_id,
            match outcome {
                Ok(()) => json!({"status":"completed"}),
                Err(e) => json!({"status":"failed","error":e.to_string()}),
            },
        );
    });
    Ok(result)
}

#[cfg(all(test, windows))]
mod tests {
    #[tokio::test]
    async fn dpapi_roundtrip_and_rotation() {
        let d = tempfile::tempdir().unwrap();
        let app = crate::App::new(Some(d.path().into())).await.unwrap();
        super::set_secret(&app, "test", "a-secret").unwrap();
        assert_eq!(super::get_secret(&app, "test").unwrap(), "a-secret");
        let bytes = std::fs::read(d.path().join("vault/test.bin")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("a-secret"));
        super::set_secret(&app, "test", "rotated").unwrap();
        assert_eq!(super::get_secret(&app, "test").unwrap(), "rotated");
        super::delete_secret(&app, "test").unwrap();
        assert!(super::get_secret(&app, "test").is_err());
    }
}
