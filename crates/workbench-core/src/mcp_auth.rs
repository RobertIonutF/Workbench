//! Host-owned authentication for HTTP MCP servers.
//!
//! Implements the public-client DCR subset of MCP authorization. CIMD-only,
//! pre-registered-only and confidential-client servers return an explicit error.
//! References: MCP 2026-07-28 authorization, discovery and security documents.

use crate::{
    App,
    app::{id, now, text},
    auth,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};
use url::Url;

const MAX_METADATA: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
struct Discovery {
    destination: String,
    resource: String,
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
    scope: String,
    supports_refresh: bool,
    require_response_issuer: bool,
}

fn credential_key(plugin_id: &str, server_name: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(plugin_id.as_bytes());
    digest.update([0]);
    digest.update(server_name.as_bytes());
    format!("mcp-{:x}", digest.finalize())
}

fn endpoint(value: &str, allow_local_http: bool) -> Result<Url> {
    let parsed = Url::parse(value).context("MCP authentication needs an absolute endpoint URL")?;
    let local = matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    );
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || !(parsed.scheme() == "https" || (allow_local_http && local && parsed.scheme() == "http"))
    {
        bail!(
            "MCP authentication endpoints require HTTPS, no user information and no fragment (HTTP is allowed only for the selected loopback resource)"
        )
    }
    Ok(parsed)
}

fn destination(value: &str) -> Result<String> {
    let parsed = endpoint(value, true)?;
    let mut result = parsed.to_string();
    if parsed.path() == "/" && parsed.query().is_none() {
        result.pop();
    }
    Ok(result)
}

fn assert_destination(expected: &str, actual: &str) -> Result<()> {
    if destination(expected)? != destination(actual)? {
        bail!(
            "MCP server URL changed. Reconnect credentials for this exact destination; stored credentials were not forwarded"
        )
    }
    Ok(())
}

fn configured_server(app: &App, plugin_id: &str, server_name: &str) -> Result<Value> {
    let plugin = app
        .store
        .get("plugin", plugin_id)
        .context("Plugin is no longer installed")?;
    let server = plugin["servers"]
        .as_array()
        .context("Plugin has no MCP servers")?
        .iter()
        .find(|s| text(s, "name") == server_name)
        .context("MCP server was not found in this plugin")?
        .clone();
    if !matches!(text(&server, "type"), "streamable-http" | "sse") {
        bail!(
            "This authentication flow is for HTTP MCP servers; stdio credentials are configured separately"
        )
    }
    destination(text(&server, "url"))?;
    Ok(server)
}

fn auth_record(app: &App, key: &str) -> Result<Option<Value>> {
    Ok(app
        .store
        .all("mcp_auth")?
        .into_iter()
        .find(|v| text(v, "id") == key))
}

fn persist(
    app: &App,
    key: &str,
    plugin_id: &str,
    server_name: &str,
    credential: &Value,
) -> Result<()> {
    auth::set_secret(app, key, &credential.to_string())?;
    app.store.put(
        "mcp_auth",
        &json!({
            "id":key,"pluginId":plugin_id,"serverName":server_name,
            "destination":credential["destination"],"kind":credential["kind"],"updatedAt":now()
        }),
    )
}

/// Save a host-owned bearer token. An empty token disconnects host credentials.
/// Credentials never become part of a plugin package or public server object.
pub fn save_key(app: &App, plugin_id: &str, server_name: &str, api_key: &str) -> Result<()> {
    let _guard = app
        .refresh_lock
        .try_lock()
        .context("A credential refresh is in progress; retry saving in a moment")?;
    let server = configured_server(app, plugin_id, server_name)?;
    let key = credential_key(plugin_id, server_name);
    app.store.delete("mcp_auth_attempt", &key)?;
    if api_key.trim().is_empty() {
        auth::delete_secret(app, &key)?;
        app.store.delete("mcp_auth", &key)?;
        return Ok(());
    }
    validate_bearer(api_key.trim())?;
    persist(
        app,
        &key,
        plugin_id,
        server_name,
        &json!({
            "kind":"api_key","destination":destination(text(&server,"url"))?,
            "access_token":api_key.trim()
        }),
    )
}

fn validate_bearer(token: &str) -> Result<()> {
    if token.is_empty() {
        bail!("The MCP server did not issue an access token")
    }
    reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .context("MCP credential contains invalid header characters")?;
    Ok(())
}

async fn json_response(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        bail!(
            "MCP authorization service returned HTTP {status}; check whether this server supports public desktop clients"
        )
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > MAX_METADATA {
            bail!("MCP authorization response exceeded its size limit")
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("MCP authorization service returned invalid JSON")
}

fn well_known(resource: &Url, suffix: &str, include_path: bool) -> Url {
    let mut url = resource.clone();
    let tail = if include_path && resource.path() != "/" {
        resource.path()
    } else {
        ""
    };
    url.set_path(&format!("/.well-known/{suffix}{tail}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn resource_metadata_urls(resource: &Url) -> Vec<Url> {
    let mut values = vec![well_known(resource, "oauth-protected-resource", true)];
    let root = well_known(resource, "oauth-protected-resource", false);
    if values[0] != root {
        values.push(root);
    }
    values
}

fn issuer_metadata_urls(issuer: &str) -> Result<Vec<Url>> {
    let parsed = endpoint(issuer, false)?;
    if parsed.query().is_some() {
        bail!("Authorization server issuer must not contain a query")
    }
    let mut result = vec![
        well_known(&parsed, "oauth-authorization-server", true),
        well_known(&parsed, "openid-configuration", true),
    ];
    if parsed.path() != "/" {
        let mut appended = parsed.clone();
        appended.set_path(&format!(
            "{}/.well-known/openid-configuration",
            parsed.path().trim_end_matches('/')
        ));
        if !result.contains(&appended) {
            result.push(appended);
        }
    }
    Ok(result)
}

fn split_header(value: &str) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' && quoted {
            current.push(c);
            escaped = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
        }
        if c == ',' && !quoted {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    if quoted || escaped {
        bail!("Malformed MCP WWW-Authenticate challenge")
    }
    parts.push(current);
    Ok(parts)
}

fn parse_parameter(value: &str) -> Result<Option<(String, String)>> {
    let Some((key, raw)) = value.split_once('=') else {
        return Ok(None);
    };
    let raw = raw.trim();
    let decoded = if raw.starts_with('"') {
        if raw.len() < 2 || !raw.ends_with('"') {
            bail!("Malformed MCP authorization challenge parameter")
        }
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::new();
        let mut escaped = false;
        for c in inner.chars() {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                bail!("Malformed MCP authorization challenge quoting")
            } else {
                out.push(c);
            }
        }
        if escaped {
            bail!("Malformed MCP authorization challenge escaping")
        }
        out
    } else {
        raw.to_string()
    };
    Ok(Some((key.trim().to_ascii_lowercase(), decoded)))
}

fn bearer_challenge(value: &str) -> Result<HashMap<String, String>> {
    let mut active = false;
    let mut found = HashMap::new();
    for part in split_header(value)? {
        let part = part.trim();
        let mut pieces = part.splitn(2, char::is_whitespace);
        let first = pieces.next().unwrap_or("");
        let rest = pieces.next().unwrap_or("").trim();
        let parameter = if first.eq_ignore_ascii_case("bearer") {
            active = true;
            rest
        } else if !first.contains('=') {
            active = false;
            continue;
        } else {
            part
        };
        if active
            && let Some((key, val)) = parse_parameter(parameter)?
            && found.insert(key, val).is_some()
        {
            bail!("Ambiguous duplicate MCP authorization challenge parameter")
        }
    }
    Ok(found)
}

fn validate_resource(metadata: &Value, expected: &str) -> Result<Vec<String>> {
    assert_destination(expected, text(metadata, "resource"))?;
    let servers = metadata["authorization_servers"]
        .as_array()
        .context("Protected resource metadata has no authorization_servers")?;
    if servers.is_empty() {
        bail!("Protected resource metadata lists no authorization server")
    }
    let mut result = Vec::new();
    for value in servers {
        let issuer = value
            .as_str()
            .context("Invalid authorization server identifier")?;
        issuer_metadata_urls(issuer)?;
        result.push(issuer.into());
    }
    Ok(result)
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn validate_issuer(metadata: &Value, expected: &str) -> Result<()> {
    // Issuer comparison is deliberately exact; URL normalization would weaken
    // RFC 8414 discovery validation and RFC 9207 mix-up protection.
    if text(metadata, "issuer") != expected {
        bail!("Authorization server metadata issuer did not match its discovery identifier")
    }
    if metadata
        .get("authorization_response_iss_parameter_supported")
        .is_some_and(|value| !value.is_boolean())
    {
        bail!("Authorization server metadata has an invalid response-issuer capability")
    }
    if !string_array(&metadata["code_challenge_methods_supported"])
        .iter()
        .any(|v| v == "S256")
    {
        bail!(
            "This authorization server does not advertise PKCE S256; Workbench will not start an insecure sign-in"
        )
    }
    for name in ["authorization_endpoint", "token_endpoint"] {
        endpoint(text(metadata, name), false)?;
    }
    if let Some(methods) = metadata.get("token_endpoint_auth_methods_supported")
        && !string_array(methods).iter().any(|v| v == "none")
    {
        bail!(
            "This MCP authorization server requires confidential-client authentication; public desktop clients are unsupported"
        )
    }
    if let Some(grants) = metadata.get("grant_types_supported")
        && !string_array(grants)
            .iter()
            .any(|v| v == "authorization_code")
    {
        bail!("This MCP authorization server does not support authorization-code sign-in")
    }
    if text(metadata, "registration_endpoint").is_empty() {
        bail!(
            "This server requires a pre-registered client or hosted Client ID Metadata Document. Workbench currently supports public dynamic registration; no client identity was invented"
        )
    }
    endpoint(text(metadata, "registration_endpoint"), false)?;
    Ok(())
}

async fn fetch_metadata(app: &App, urls: Vec<Url>) -> Result<Value> {
    for url in urls {
        let response = app
            .client
            .get(url)
            .timeout(REQUEST_TIMEOUT)
            .header("Accept", "application/json")
            .send()
            .await?;
        if matches!(response.status().as_u16(), 404 | 405) {
            continue;
        }
        // Redirects are not followed, including across origins.
        return json_response(response).await;
    }
    bail!("This MCP service did not publish supported OAuth discovery metadata")
}

async fn discover(app: &App, target: &str, previous: Option<&Value>) -> Result<Discovery> {
    let resource_url = endpoint(target, true)?;
    let dest = destination(target)?;
    let response = app
        .client
        .get(resource_url.clone())
        .timeout(REQUEST_TIMEOUT)
        .header("Accept", "application/json, text/event-stream")
        .send()
        .await?;
    if response.status().is_redirection() {
        bail!(
            "MCP server redirected during authorization discovery; configure its final URL explicitly"
        )
    }
    let mut challenge = HashMap::new();
    for header in response
        .headers()
        .get_all(reqwest::header::WWW_AUTHENTICATE)
    {
        let params = bearer_challenge(
            header
                .to_str()
                .context("Invalid MCP authorization header")?,
        )?;
        for (key, val) in params {
            if challenge.insert(key, val).is_some() {
                bail!("Ambiguous MCP authorization challenge headers")
            }
        }
    }
    drop(response);
    let metadata_urls = if let Some(advertised) = challenge.get("resource_metadata") {
        let metadata_url = endpoint(advertised, true)?;
        if metadata_url.scheme() == "http" && metadata_url.origin() != resource_url.origin() {
            bail!("Insecure cross-origin protected resource metadata is unsupported")
        }
        vec![metadata_url]
    } else {
        resource_metadata_urls(&resource_url)
    };
    let protected = fetch_metadata(app, metadata_urls).await?;
    let issuers = validate_resource(&protected, &dest)?;
    let issuer = previous
        .and_then(|old| old["issuer"].as_str())
        .filter(|old| issuers.iter().any(|v| v == old))
        .unwrap_or(&issuers[0])
        .to_string();
    let metadata = fetch_metadata(app, issuer_metadata_urls(&issuer)?).await?;
    validate_issuer(&metadata, &issuer)?;
    let mut scopes: BTreeSet<String> = challenge
        .get("scope")
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_else(|| {
            string_array(&protected["scopes_supported"])
                .into_iter()
                .collect()
        });
    if let Some(old) = previous
        && text(old, "issuer") == issuer
        && text(old, "destination") == dest
    {
        scopes.extend(text(old, "scope").split_whitespace().map(str::to_owned));
    }
    if string_array(&metadata["scopes_supported"])
        .iter()
        .any(|s| s == "offline_access")
    {
        scopes.insert("offline_access".into());
    }
    let supports_refresh = metadata.get("grant_types_supported").is_none()
        || string_array(&metadata["grant_types_supported"])
            .iter()
            .any(|s| s == "refresh_token");
    Ok(Discovery {
        destination: dest.clone(),
        resource: dest,
        issuer,
        authorization_endpoint: text(&metadata, "authorization_endpoint").into(),
        token_endpoint: text(&metadata, "token_endpoint").into(),
        registration_endpoint: text(&metadata, "registration_endpoint").into(),
        scope: scopes.into_iter().collect::<Vec<_>>().join(" "),
        supports_refresh,
        require_response_issuer: metadata["authorization_response_iss_parameter_supported"] == true,
    })
}

fn pkce(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn callback_parameters(raw: &str) -> Result<HashMap<String, String>> {
    let mut result = HashMap::new();
    for (key, val) in url::form_urlencoded::parse(raw.as_bytes()) {
        if result.insert(key.into_owned(), val.into_owned()).is_some() {
            bail!("Duplicate OAuth callback parameter")
        }
    }
    Ok(result)
}

fn validate_callback(
    params: &HashMap<String, String>,
    state: &str,
    issuer: &str,
    require_issuer: bool,
) -> Result<String> {
    if params.get("state").map(String::as_str) != Some(state) {
        bail!("MCP OAuth state validation failed")
    }
    if let Some(actual) = params.get("iss") {
        if actual != issuer {
            bail!("MCP OAuth response issuer did not match the selected authorization server")
        }
    } else if require_issuer {
        bail!("MCP OAuth response omitted its required issuer")
    }
    // Validate issuer before observing provider-supplied errors.
    if params.contains_key("error") {
        bail!("MCP sign-in was declined or could not be completed")
    }
    let code = params
        .get("code")
        .filter(|v| !v.is_empty())
        .context("MCP sign-in did not return an authorization code")?;
    Ok(code.clone())
}

fn update_tokens(credential: &mut Value, response: &Value) -> Result<()> {
    if !text(response, "token_type").eq_ignore_ascii_case("bearer") {
        bail!("MCP authorization service returned an unsupported token type")
    }
    validate_bearer(text(response, "access_token"))?;
    credential["access_token"] = response["access_token"].clone();
    for key in ["refresh_token", "scope"] {
        if response[key].is_string() {
            credential[key] = response[key].clone();
        }
    }
    credential["expiresAt"] = if let Some(expires) = response.get("expires_in") {
        let seconds = expires
            .as_i64()
            .filter(|v| *v > 0)
            .context("Invalid MCP access-token expiry")?;
        json!(
            chrono::Utc::now()
                .timestamp()
                .checked_add(seconds)
                .context("Invalid MCP access-token expiry")?
        )
    } else {
        Value::Null
    };
    Ok(())
}

/// Start a genuine PKCE authorization flow for an installed HTTP MCP server.
pub async fn start(app: Arc<App>, plugin_id: &str, server_name: &str) -> Result<Value> {
    let server = configured_server(&app, plugin_id, server_name)?;
    let target = destination(text(&server, "url"))?;
    let key = credential_key(plugin_id, server_name);
    let previous = if auth_record(&app, &key)?.is_some() {
        serde_json::from_str::<Value>(&auth::get_secret(&app, &key)?).ok()
    } else {
        None
    };
    let discovery = discover(&app, &target, previous.as_ref()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!(
        "http://127.0.0.1:{}/oauth/mcp/callback",
        listener.local_addr()?.port()
    );
    let mut grants = vec!["authorization_code"];
    if discovery.supports_refresh {
        grants.push("refresh_token");
    }
    let registration = json_response(app.client.post(&discovery.registration_endpoint).timeout(REQUEST_TIMEOUT).json(&json!({
        "client_name":"Workbench","redirect_uris":[redirect],"token_endpoint_auth_method":"none",
        "grant_types":grants,"response_types":["code"],"application_type":"native"
    })).send().await?).await?;
    let client_id = text(&registration, "client_id").to_owned();
    if client_id.is_empty() {
        bail!("MCP dynamic registration returned no client ID")
    }
    if registration["token_endpoint_auth_method"]
        .as_str()
        .is_some_and(|v| v != "none")
    {
        bail!(
            "MCP registration requires a confidential client; desktop public-client sign-in is unsupported"
        )
    }
    if let Some(registered) = registration["redirect_uris"].as_array()
        && !registered
            .iter()
            .any(|value| value.as_str() == Some(&redirect))
    {
        bail!("MCP registration did not accept the exact loopback redirect URI")
    }
    let state = format!("{}{}", id(), id());
    let verifier = format!("{}{}", id(), id()).replace('-', "");
    let login_id = id();
    let mut authorization = endpoint(&discovery.authorization_endpoint, false)?;
    // Do not allow fixed endpoint query parameters to shadow OAuth values.
    let reserved = [
        "client_id",
        "redirect_uri",
        "response_type",
        "state",
        "scope",
        "resource",
        "code_challenge",
        "code_challenge_method",
    ];
    if authorization
        .query_pairs()
        .any(|(k, _)| reserved.contains(&k.as_ref()))
    {
        bail!("Authorization endpoint contains conflicting fixed OAuth parameters")
    }
    authorization.query_pairs_mut().extend_pairs([
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("response_type", "code"),
        ("state", state.as_str()),
        ("resource", discovery.resource.as_str()),
        ("code_challenge_method", "S256"),
        ("code_challenge", pkce(&verifier).as_str()),
    ]);
    if !discovery.scope.is_empty() {
        authorization
            .query_pairs_mut()
            .append_pair("scope", &discovery.scope);
    }
    app.store.put("mcp_auth_attempt",&json!({"id":key,"loginId":login_id,"destination":discovery.destination,"issuer":discovery.issuer}))?;
    app.oauth.lock().insert(
        login_id.clone(),
        json!({"status":"pending","pluginId":plugin_id,"serverName":server_name}),
    );
    let result = json!({"url":authorization.as_str(),"loginId":login_id,"issuer":discovery.issuer,"scope":discovery.scope});
    let plugin_id = plugin_id.to_owned();
    let server_name = server_name.to_owned();
    tokio::spawn(async move {
        let (tx, rx) = tokio::sync::oneshot::channel::<HashMap<String, String>>();
        let sender = Arc::new(parking_lot::Mutex::new(Some(tx)));
        let callback_state = state.clone();
        let router = axum::Router::new().route("/oauth/mcp/callback",axum::routing::get(move |axum::extract::RawQuery(raw):axum::extract::RawQuery| {
            let sender = sender.clone(); let expected = callback_state.clone();
            async move {
                let Ok(params) = callback_parameters(raw.as_deref().unwrap_or("")) else { return (axum::http::StatusCode::BAD_REQUEST,"Invalid sign-in callback.") };
                if params.get("state") != Some(&expected) { return (axum::http::StatusCode::BAD_REQUEST,"Invalid sign-in state.") }
                if let Some(tx) = sender.lock().take() { let _ = tx.send(params); }
                (axum::http::StatusCode::OK,"Return to Workbench to see the connection result. You can close this tab.")
            }
        }));
        let stop = tokio_util::sync::CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(stopping.cancelled_owned())
                .await;
        });
        let outcome: Result<()> = async {
            let params = tokio::time::timeout(Duration::from_secs(300),rx).await.context("MCP sign-in timed out")??;
            let code = validate_callback(&params,&state,&discovery.issuer,discovery.require_response_issuer)?;
            let token = json_response(app.client.post(&discovery.token_endpoint).timeout(REQUEST_TIMEOUT).form(&[
                ("grant_type","authorization_code"),("code",code.as_str()),("client_id",client_id.as_str()),
                ("redirect_uri",redirect.as_str()),("code_verifier",verifier.as_str()),("resource",discovery.resource.as_str())
            ]).send().await?).await?;
            let mut credential = json!({"kind":"oauth","destination":discovery.destination,"resource":discovery.resource,
                "issuer":discovery.issuer,"client_id":client_id,"token_endpoint":discovery.token_endpoint,"scope":discovery.scope});
            update_tokens(&mut credential,&token)?;
            let _guard = app.refresh_lock.lock().await;
            let attempt = app.store.get("mcp_auth_attempt",&key).context("MCP sign-in was superseded or disconnected")?;
            if text(&attempt,"loginId") != login_id { bail!("This MCP sign-in was superseded by a newer attempt") }
            let current = configured_server(&app,&plugin_id,&server_name)?;
            assert_destination(text(&credential,"destination"),text(&current,"url"))?;
            persist(&app,&key,&plugin_id,&server_name,&credential)?;
            app.store.delete("mcp_auth_attempt",&key)?;
            Ok(())
        }.await;
        stop.cancel();
        let _ = task.await;
        app.oauth.lock().insert(login_id,match outcome {
            Ok(()) => json!({"status":"completed","pluginId":plugin_id,"serverName":server_name}),
            Err(error) => json!({"status":"failed","error":error.to_string(),"pluginId":plugin_id,"serverName":server_name})
        });
    });
    Ok(result)
}

/// Return private host-owned headers only. Callers must merge without exposing
/// them to model context and must not forward them through a redirect.
pub async fn headers(app: &App, plugin_id: &str, server: &Value) -> Result<Value> {
    if !matches!(text(server, "type"), "streamable-http" | "sse") {
        return Ok(json!({}));
    }
    let server_name = text(server, "name");
    let key = credential_key(plugin_id, server_name);
    let _guard = app.refresh_lock.lock().await;
    let Some(record) = auth_record(app, &key)? else {
        return Ok(json!({}));
    };
    assert_destination(text(&record, "destination"), text(server, "url"))?;
    let mut credential: Value = serde_json::from_str(&auth::get_secret(app, &key)?)
        .context("Stored MCP credentials are unreadable; reconnect this server")?;
    assert_destination(text(&credential, "destination"), text(server, "url"))?;
    let expired = credential["expiresAt"]
        .as_i64()
        .is_some_and(|t| t <= chrono::Utc::now().timestamp() + 30);
    if text(&credential, "kind") == "oauth" && expired {
        let refresh_token = text(&credential, "refresh_token");
        if refresh_token.is_empty() {
            bail!(
                "MCP authorization expired and no refresh token was issued; sign in to this server again"
            )
        }
        let token_endpoint = endpoint(text(&credential, "token_endpoint"), false)?;
        let response = app
            .client
            .post(token_endpoint)
            .timeout(REQUEST_TIMEOUT)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", text(&credential, "client_id")),
                ("resource", text(&credential, "resource")),
            ])
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("MCP authorization expired or was revoked; reconnect this server")
        }
        let renewed = json_response(response).await?;
        update_tokens(&mut credential, &renewed)?;
        persist(app, &key, plugin_id, server_name, &credential)?;
    }
    let token = text(&credential, "access_token");
    validate_bearer(token)?;
    Ok(json!({"Authorization":format!("Bearer {token}")}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issuer_document() -> Value {
        json!({"issuer":"https://auth.example/tenant","authorization_endpoint":"https://auth.example/authorize","token_endpoint":"https://auth.example/token","registration_endpoint":"https://auth.example/register","code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]})
    }

    #[test]
    fn discovery_order_preserves_paths() {
        let urls = resource_metadata_urls(
            &Url::parse("https://tools.example/public/mcp?tenant=a").unwrap(),
        );
        assert_eq!(
            urls[0].as_str(),
            "https://tools.example/.well-known/oauth-protected-resource/public/mcp"
        );
        assert_eq!(
            urls[1].as_str(),
            "https://tools.example/.well-known/oauth-protected-resource"
        );
        let urls = issuer_metadata_urls("https://auth.example/tenant").unwrap();
        assert_eq!(
            urls[0].as_str(),
            "https://auth.example/.well-known/oauth-authorization-server/tenant"
        );
        assert_eq!(
            urls[1].as_str(),
            "https://auth.example/.well-known/openid-configuration/tenant"
        );
        assert_eq!(
            urls[2].as_str(),
            "https://auth.example/tenant/.well-known/openid-configuration"
        );
        let trailing = resource_metadata_urls(&Url::parse("https://tools.example/mcp/").unwrap());
        assert_eq!(
            trailing[0].as_str(),
            "https://tools.example/.well-known/oauth-protected-resource/mcp/"
        );
    }

    #[test]
    fn validates_metadata_and_public_pkce_support() {
        let valid = issuer_document();
        assert!(validate_issuer(&valid, "https://auth.example/tenant").is_ok());
        assert!(validate_issuer(&valid, "https://AUTH.example/tenant").is_err());
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .remove("code_challenge_methods_supported");
        assert!(validate_issuer(&invalid, "https://auth.example/tenant").is_err());
        invalid = valid.clone();
        invalid["token_endpoint_auth_methods_supported"] = json!(["client_secret_basic"]);
        assert!(validate_issuer(&invalid, "https://auth.example/tenant").is_err());
        invalid = valid;
        invalid
            .as_object_mut()
            .unwrap()
            .remove("registration_endpoint");
        assert!(validate_issuer(&invalid, "https://auth.example/tenant").is_err());
        assert!(validate_resource(&json!({"resource":"https://other.example/mcp","authorization_servers":["https://auth.example"]}),"https://tools.example/mcp").is_err());
    }

    #[test]
    fn parses_bearer_challenges_without_quoted_comma_confusion() {
        let params = bearer_challenge("Basic realm=\"other\", Bearer realm=\"one,two\", resource_metadata=\"https://tools.example/meta\", scope=\"files:read files:write\"").unwrap();
        assert_eq!(params["resource_metadata"], "https://tools.example/meta");
        assert_eq!(params["scope"], "files:read files:write");
        assert!(bearer_challenge("Bearer scope=\"a\", scope=\"b\"").is_err());
        assert!(bearer_challenge("Bearer scope=\"unterminated").is_err());
    }

    #[test]
    fn verifies_pkce_and_callback_state_issuer_and_uniqueness() {
        assert_eq!(
            pkce("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let params =
            callback_parameters("state=expected&code=one&iss=https%3A%2F%2Fauth.example%2Ftenant")
                .unwrap();
        assert!(
            validate_callback(&params, "expected", "https://auth.example/tenant", true).is_ok()
        );
        assert!(validate_callback(&params, "other", "https://auth.example/tenant", true).is_err());
        assert!(
            validate_callback(&params, "expected", "https://auth.example/tenant/", true).is_err()
        );
        let no_issuer = callback_parameters("state=expected&code=one").unwrap();
        assert!(
            validate_callback(&no_issuer, "expected", "https://auth.example/tenant", true).is_err()
        );
        assert!(
            validate_callback(&no_issuer, "expected", "https://auth.example/tenant", false).is_ok()
        );
        assert!(callback_parameters("state=one&state=two&code=a").is_err());
    }

    #[test]
    fn destination_and_token_rotation_are_strict() {
        assert!(
            assert_destination(
                "https://tools.example/mcp?a=1",
                "https://tools.example/mcp?a=2"
            )
            .is_err()
        );
        assert!(assert_destination("https://tools.example/a", "https://tools.example/b").is_err());
        assert!(endpoint("http://auth.example/token", false).is_err());
        assert!(endpoint("https://user:pass@auth.example/token", false).is_err());
        assert!(validate_bearer("token\r\nOther: injected").is_err());
        let mut credential = json!({"refresh_token":"old"});
        update_tokens(&mut credential,&json!({"access_token":"new","token_type":"Bearer","refresh_token":"rotated","expires_in":60})).unwrap();
        assert_eq!(credential["refresh_token"], "rotated");
        assert!(credential["expiresAt"].as_i64().unwrap() > chrono::Utc::now().timestamp());
        assert_ne!(credential_key("a", "b-c"), credential_key("a-b", "c"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn api_token_is_vaulted_bound_and_disconnectable() {
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(Some(temp.path().into())).await.unwrap();
        let server =
            json!({"name":"private","type":"streamable-http","url":"https://tools.example/mcp"});
        app.store
            .put("plugin", &json!({"id":"test-plugin","servers":[server]}))
            .unwrap();
        save_key(&app, "test-plugin", "private", "fixture-token").unwrap();
        assert_eq!(
            headers(&app, "test-plugin", &server).await.unwrap()["Authorization"],
            "Bearer fixture-token"
        );
        let mut changed = server.clone();
        changed["url"] = json!("https://tools.example/another");
        assert!(headers(&app, "test-plugin", &changed).await.is_err());
        save_key(&app, "test-plugin", "private", "").unwrap();
        assert_eq!(
            headers(&app, "test-plugin", &server).await.unwrap(),
            json!({})
        );
    }
}
