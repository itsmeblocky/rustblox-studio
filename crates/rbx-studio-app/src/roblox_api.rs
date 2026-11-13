//! Roblox web access for Rustblox Studio.
//!
//! Two separate concerns live here:
//!
//! 1. **Public Toolbox metadata** (search → item details → thumbnails). These
//!    endpoints need no authentication and are called anonymously with an
//!    honest `RustbloxStudio` User-Agent.
//! 2. **Official OAuth 2.0 login** (Authorization Code + PKCE, confidential
//!    client). The system browser handles the credentials; we only receive a
//!    loopback callback and exchange the code at `apis.roblox.com/oauth/v1`.
//!
//! No client impersonation: we never pretend to be the Roblox Studio client,
//! and no `.ROBLOSECURITY` cookie is read or written.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

/// Sent with every request so Roblox traffic is attributable.
pub const USER_AGENT: &str = "RustbloxStudio/0.1 (desktop studio; anonymous metadata)";

const TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Toolbox (public, unauthenticated)
// ---------------------------------------------------------------------------

/// One Toolbox search hit, already joined with details and a thumbnail.
#[derive(Debug, Clone, Serialize)]
pub struct ToolboxItem {
    pub id: u64,
    pub name: String,
    pub creator: String,
    pub creator_id: u64,
    pub asset_type: u32,
    pub description: String,
    pub votes: i64,
    pub up_percent: f64,
    pub thumbnail: String,
    pub free: bool,
    pub has_scripts: bool,
}

/// Toolbox categories that `toolbox-service` actually accepts.
pub fn categories() -> Value {
    serde_json::json!([
        { "id": 10, "label": "Models" },
        { "id": 40, "label": "Meshes" },
        { "id": 3, "label": "Audio" },
        { "id": 13, "label": "Images" },
        { "id": 38, "label": "Plugins" },
        { "id": 24, "label": "Animations" }
    ])
}

fn http_json(url: &str) -> Result<Value> {
    let response = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/json")
        .timeout(TIMEOUT)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                anyhow!("HTTP {code} from {url}: {}", truncate(&body, 300))
            }
            other => anyhow!("request failed for {url}: {other}"),
        })?;
    let text = response
        .into_string()
        .with_context(|| format!("reading response body from {url}"))?;
    serde_json::from_str(&text).with_context(|| format!("parsing JSON from {url}"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// `percent-encoding` for query values (RFC 3986 unreserved set).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(target: &str) -> HashMap<String, String> {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(urldecode(key), urldecode(value));
    }
    map
}

/// Searches a Toolbox category and enriches the hits with details + thumbnails.
///
/// `keyword` may be empty to browse the category's default listing.
pub fn search(category: u32, keyword: &str, limit: usize) -> Result<Vec<ToolboxItem>> {
    let limit = limit.clamp(1, 30);
    let mut url = format!(
        "https://apis.roblox.com/toolbox-service/v1/marketplace/{category}?limit={limit}"
    );
    let keyword = keyword.trim();
    if !keyword.is_empty() {
        url.push_str(&format!("&keyword={}", urlencode(keyword)));
    }
    let listing = http_json(&url)?;

    let ids: Vec<u64> = listing
        .get("data")
        .and_then(|d| d.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(|v| v.as_u64()))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let details = item_details(&ids)?;
    let thumbs = thumbnails(&ids)?;

    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(detail) = details.get(&id) else { continue };
        let asset = detail.get("asset").cloned().unwrap_or(Value::Null);
        let creator = detail.get("creator").cloned().unwrap_or(Value::Null);
        let voting = detail.get("voting").cloned().unwrap_or(Value::Null);
        let fiat = detail.get("fiatProduct").cloned().unwrap_or(Value::Null);
        out.push(ToolboxItem {
            id,
            name: str_of(&asset, "name").unwrap_or_else(|| format!("Asset {id}")),
            creator: str_of(&creator, "name").unwrap_or_else(|| "Unknown".to_string()),
            creator_id: u64_of(&creator, "id").unwrap_or(0),
            asset_type: u64_of(&asset, "typeId").unwrap_or(0) as u32,
            description: str_of(&asset, "description").unwrap_or_default(),
            votes: i64_of(&voting, "voteCount").unwrap_or(0),
            up_percent: f64_of(&voting, "upVotePercent").unwrap_or(0.0),
            thumbnail: thumbs.get(&id).cloned().unwrap_or_default(),
            free: fiat
                .get("isFree")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            has_scripts: asset
                .get("hasScripts")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }
    Ok(out)
}

fn item_details(ids: &[u64]) -> Result<HashMap<u64, Value>> {
    let mut map = HashMap::new();
    for chunk in ids.chunks(20) {
        let list = chunk
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("https://apis.roblox.com/toolbox-service/v1/items/details?assetIds={list}");
        let value = http_json(&url)?;
        if let Some(rows) = value.get("data").and_then(|d| d.as_array()) {
            for row in rows {
                if let Some(id) = row.get("asset").and_then(|a| a.get("id")).and_then(|v| v.as_u64()) {
                    map.insert(id, row.clone());
                }
            }
        }
    }
    Ok(map)
}

fn thumbnails(ids: &[u64]) -> Result<HashMap<u64, String>> {
    let mut map = HashMap::new();
    for chunk in ids.chunks(50) {
        let list = chunk
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!(
            "https://thumbnails.roblox.com/v1/assets?assetIds={list}&size=150x150&format=Png&isCircular=false"
        );
        let Ok(value) = http_json(&url) else {
            // Thumbnails are decorative: a failure must not lose the results.
            continue;
        };
        if let Some(rows) = value.get("data").and_then(|d| d.as_array()) {
            for row in rows {
                let id = row.get("targetId").and_then(|v| v.as_u64());
                let state = row.get("state").and_then(|v| v.as_str()).unwrap_or("");
                let image = row.get("imageUrl").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(id) = id {
                    if state == "Completed" && !image.is_empty() {
                        map.insert(id, image.to_string());
                    }
                }
            }
        }
    }
    Ok(map)
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn u64_of(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}

fn i64_of(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64())
}

fn f64_of(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64())
}

// ---------------------------------------------------------------------------
// OAuth 2.0 (Authorization Code + PKCE, confidential client)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl OAuthConfig {
    /// Loads `secrets/roblox_oauth.json` (gitignored; see `AGENTS.md`).
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "reading OAuth credentials from {} (create the file as described in AGENTS.md)",
                path.display()
            )
        })?;
        let cfg: OAuthConfig = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        if cfg.client_id.trim().is_empty() {
            return Err(anyhow!(
                "{} has an empty client_id — fill it in from the Roblox Creator Dashboard",
                path.display()
            ));
        }
        Ok(cfg)
    }

    fn scope(&self) -> String {
        if self.scopes.is_empty() {
            "openid profile".to_string()
        } else {
            self.scopes.join(" ")
        }
    }
}

/// Identity of the signed-in user. The refresh token never leaves this process
/// except into the OS credential store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub user_id: u64,
    pub name: String,
    pub display_name: String,
    pub avatar_url: String,
    pub refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
}

fn random_b64url(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn split_redirect(redirect: &str) -> Result<(String, u16, String)> {
    let rest = redirect
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("redirect_uri must be an http:// loopback URL: {redirect}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("redirect_uri needs an explicit port: {redirect}"))?;
    let port: u16 = port
        .parse()
        .with_context(|| format!("invalid port in redirect_uri: {redirect}"))?;
    Ok((host.to_string(), port, path.to_string()))
}

/// Runs the interactive login. Blocks until the browser callback arrives.
///
/// `status` is called with human-readable progress for the UI.
pub fn login(cfg: &OAuthConfig, status: &mut dyn FnMut(&str)) -> Result<Session> {
    let verifier = random_b64url(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_hex(16);

    let (host, port, _path) = split_redirect(&cfg.redirect_uri)?;
    // `localhost` may resolve to ::1 first on Windows; bind the IPv4 loopback
    // explicitly (browsers fall back to it) so the callback is always caught.
    let bind_host = if host.eq_ignore_ascii_case("localhost") {
        "127.0.0.1"
    } else {
        host.as_str()
    };
    let listener = TcpListener::bind((bind_host, port)).with_context(|| {
        format!("cannot listen on {bind_host}:{port} — is another login already in progress?")
    })?;

    let authorize = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        cfg.authorize_url,
        urlencode(&cfg.client_id),
        urlencode(&cfg.redirect_uri),
        urlencode(&cfg.scope()),
        urlencode(&state),
        urlencode(&challenge),
    );

    status("Opening the Roblox sign-in page in your browser…");
    open_browser(&authorize)?;
    status("Waiting for Roblox to redirect back to 127.0.0.1…");

    // The authorization page can fail without ever redirecting (for example when
    // the app is missing a requested scope), so never block on `accept` forever.
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let (mut stream, _peer) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "timed out waiting for the sign-in callback. If the browser showed a Roblox \
                         error page (for example \"Scope not allowed for this application: openid\"), \
                         open the app in the Creator Dashboard, add the openid and profile permission \
                         scopes, save, and try again"
                    ));
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(anyhow!("accepting the OAuth callback: {e}")),
        }
    };
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let target = read_request_target(&mut stream).unwrap_or_default();
    let query = parse_query(&target);

    let error = query.get("error").cloned();
    let code = query.get("code").cloned();
    let returned_state = query.get("state").cloned().unwrap_or_default();

    if let Some(err) = error {
        respond(&mut stream, "Sign-in cancelled", "Roblox reported an error; you can close this tab.");
        return Err(anyhow!("authorization denied: {err}"));
    }
    if returned_state != state {
        respond(&mut stream, "Sign-in failed", "State mismatch — close this tab and try again.");
        return Err(anyhow!("OAuth state mismatch (possible CSRF); aborting"));
    }
    let Some(code) = code else {
        respond(&mut stream, "Sign-in failed", "No authorization code was returned.");
        return Err(anyhow!("callback did not include an authorization code"));
    };
    respond(
        &mut stream,
        "Signed in to Rustblox Studio",
        "You can close this tab and return to the Studio window.",
    );

    status("Exchanging the authorization code…");
    let body = format!(
        "grant_type=authorization_code&code={}&client_id={}&client_secret={}&redirect_uri={}&code_verifier={}",
        urlencode(&code),
        urlencode(&cfg.client_id),
        urlencode(&cfg.client_secret),
        urlencode(&cfg.redirect_uri),
        urlencode(&verifier),
    );
    let token = post_token(cfg, &body)?;

    status("Reading your Roblox profile…");
    let session = fetch_identity(cfg, &token.access_token, &token.refresh_token)?;
    status("Signed in.");
    Ok(session)
}

/// Uses a stored refresh token to obtain a fresh access token.
pub fn refresh(cfg: &OAuthConfig, session: &mut Session) -> Result<String> {
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
        urlencode(&session.refresh_token),
        urlencode(&cfg.client_id),
        urlencode(&cfg.client_secret),
    );
    let token = post_token(cfg, &body)?;
    if !token.refresh_token.is_empty() {
        session.refresh_token = token.refresh_token;
    }
    Ok(token.access_token)
}

fn post_token(cfg: &OAuthConfig, body: &str) -> Result<TokenResponse> {
    let response = ureq::post(&cfg.token_url)
        .set("User-Agent", USER_AGENT)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept", "application/json")
        .timeout(TIMEOUT)
        .send_string(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let text = resp.into_string().unwrap_or_default();
                anyhow!("token endpoint returned {code}: {}", truncate(&text, 400))
            }
            other => anyhow!("token request failed: {other}"),
        })?;
    let text = response.into_string().context("reading token response")?;
    serde_json::from_str(&text).with_context(|| format!("parsing token response: {}", truncate(&text, 200)))
}

fn fetch_identity(cfg: &OAuthConfig, access_token: &str, refresh_token: &str) -> Result<Session> {
    let response = ureq::get(&cfg.userinfo_url)
        .set("User-Agent", USER_AGENT)
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("Accept", "application/json")
        .timeout(TIMEOUT)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let text = resp.into_string().unwrap_or_default();
                anyhow!("userinfo returned {code}: {}", truncate(&text, 300))
            }
            other => anyhow!("userinfo request failed: {other}"),
        })?;
    let text = response.into_string().context("reading userinfo")?;
    let info: Value = serde_json::from_str(&text).context("parsing userinfo")?;

    let user_id = info
        .get("sub")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| info.get("sub").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let name = info
        .get("preferred_username")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let display_name = info
        .get("nickname")
        .or_else(|| info.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(&name)
        .to_string();
    let avatar_url = info
        .get("picture")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Session {
        user_id,
        name,
        display_name,
        avatar_url,
        refresh_token: refresh_token.to_string(),
    })
}

fn read_request_target(stream: &mut std::net::TcpStream) -> Result<String> {
    let mut buf = [0u8; 8192];
    let mut read = 0usize;
    loop {
        let n = stream.read(&mut buf[read..]).context("reading callback request")?;
        if n == 0 {
            break;
        }
        read += n;
        if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") || read == buf.len() {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf[..read]);
    let first = text.lines().next().unwrap_or("");
    let target = first.split_whitespace().nth(1).unwrap_or("").to_string();
    Ok(target)
}

fn respond(stream: &mut std::net::TcpStream, title: &str, body: &str) {
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{title}</title>\
         <body style=\"font:16px system-ui;background:#121215;color:#e6e8ee;display:grid;place-items:center;height:100vh;margin:0\">\
         <div style=\"max-width:34rem;text-align:center\"><h1 style=\"font-size:1.4rem\">{title}</h1><p style=\"color:#a9adbb\">{body}</p></div></body>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Opens the authorize URL in the user's default browser (no embedded webview:
/// Roblox credentials must never pass through this process).
fn open_browser(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::iter::once;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        fn wide(s: &str) -> Vec<u16> {
            OsStr::new(s).encode_wide().chain(once(0)).collect()
        }
        let verb = wide("open");
        let file = wide(url);
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if (result as isize) <= 32 {
            return Err(anyhow!("could not open the default browser"));
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        std::process::Command::new(opener)
            .arg(url)
            .spawn()
            .context("launching the system browser")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Experiences (games the signed-in user can edit)
// ---------------------------------------------------------------------------

/// One editable experience, ready for the Start page grid.
#[derive(Debug, Clone, Serialize)]
pub struct Experience {
    pub universe_id: u64,
    pub root_place_id: u64,
    pub name: String,
    pub description: String,
    pub thumbnail: String,
}

/// Refreshes the stored session and returns a fresh access token. The (possibly
/// rotated) refresh token is persisted so the next call keeps working.
fn ensure_access_token(cfg: &OAuthConfig) -> Result<(Session, String)> {
    let mut session = load_session()?
        .ok_or_else(|| anyhow!("sign in with Roblox to list your experiences"))?;
    let token = refresh(cfg, &mut session)?;
    let _ = save_session(&session);
    Ok((session, token))
}

/// Universe ids the signed-in user granted this app access to during consent.
/// Requires the `universe:read` scope (a new consent is needed after adding it).
fn granted_universe_ids(cfg: &OAuthConfig, token: &str) -> Result<Vec<u64>> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let body = format!(
        "token={}&client_id={}&client_secret={}",
        urlencode(token),
        urlencode(&cfg.client_id),
        urlencode(&cfg.client_secret)
    );
    let response = agent
        .post("https://apis.roblox.com/oauth/v1/token/resources")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("User-Agent", USER_AGENT)
        .send_string(&body);
    let text = match response {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(code, r)) => {
            let t = r.into_string().unwrap_or_default();
            if code == 401 || code == 403 {
                return Err(anyhow!(
                    "Roblox rejected the session while listing experiences ({code}). Sign out and sign in again to grant the new permissions."
                ));
            }
            return Err(anyhow!("token/resources failed ({code}): {}", truncate(&t, 220)));
        }
        Err(e) => return Err(anyhow!("token/resources request failed: {e}")),
    };
    let v: Value = serde_json::from_str(&text).context("parsing token/resources")?;
    let mut ids = Vec::new();
    if let Some(infos) = v.get("resource_infos").and_then(Value::as_array) {
        for info in infos {
            if let Some(arr) = info
                .get("resources")
                .and_then(|r| r.get("universe"))
                .and_then(|u| u.get("ids"))
                .and_then(Value::as_array)
            {
                for id in arr {
                    if let Some(s) = id.as_str() {
                        if let Ok(n) = s.parse::<u64>() {
                            ids.push(n);
                        }
                    } else if let Some(n) = id.as_u64() {
                        ids.push(n);
                    }
                }
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn universe_details(ids: &[u64], token: &str) -> Result<Vec<Experience>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let joined = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let url = format!("https://games.roblox.com/v1/games?universeIds={joined}");
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let response = agent
        .get(&url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/json")
        .set("Authorization", &format!("Bearer {token}"))
        .call();
    let text = match response {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(code, r)) => {
            return Err(anyhow!(
                "fetching experience details failed ({code}): {}",
                truncate(&r.into_string().unwrap_or_default(), 220)
            ));
        }
        Err(e) => return Err(anyhow!("experience details request failed: {e}")),
    };
    let v: Value = serde_json::from_str(&text).context("parsing experience details")?;
    let mut out = Vec::new();
    if let Some(data) = v.get("data").and_then(Value::as_array) {
        for d in data {
            let universe_id = d.get("id").and_then(Value::as_u64).unwrap_or(0);
            let root_place_id = d.get("rootPlaceId").and_then(Value::as_u64).unwrap_or(0);
            if universe_id == 0 || root_place_id == 0 {
                continue;
            }
            out.push(Experience {
                universe_id,
                root_place_id,
                name: d
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Experience")
                    .to_string(),
                description: d
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                thumbnail: String::new(),
            });
        }
    }
    Ok(out)
}

fn game_thumbnails(ids: &[u64]) -> HashMap<u64, String> {
    let mut map = HashMap::new();
    if ids.is_empty() {
        return map;
    }
    let joined = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // Primary: game thumbnails (wide). Fallback: square game icons.
    let urls = [
        format!(
            "https://thumbnails.roblox.com/v1/games/multiget/thumbnails?universeIds={joined}&countPerUniverse=1&defaults=true&size=768x432&format=Png&isCircular=false"
        ),
        format!(
            "https://thumbnails.roblox.com/v1/games/icons?universeIds={joined}&size=256x256&format=Png&isCircular=false"
        ),
    ];
    for url in urls {
        if let Ok(v) = http_json(&url) {
            if let Some(data) = v.get("data").and_then(Value::as_array) {
                for entry in data {
                    let uid = entry
                        .get("universeId")
                        .or_else(|| entry.get("targetId"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if uid == 0 || map.contains_key(&uid) {
                        continue;
                    }
                    if let Some(thumbs) = entry.get("thumbnails").and_then(Value::as_array) {
                        for t in thumbs {
                            if t.get("state").and_then(Value::as_str) == Some("Completed") {
                                if let Some(img) = t.get("imageUrl").and_then(Value::as_str) {
                                    if !img.is_empty() {
                                        map.insert(uid, img.to_string());
                                        break;
                                    }
                                }
                            }
                        }
                    } else if let Some(img) = entry.get("imageUrl").and_then(Value::as_str) {
                        if entry.get("state").and_then(Value::as_str) == Some("Completed")
                            && !img.is_empty()
                        {
                            map.insert(uid, img.to_string());
                        }
                    }
                }
            }
        }
        if map.len() == ids.len() {
            break;
        }
    }
    map
}

/// Lists experiences the signed-in user granted this app access to, with
/// metadata and thumbnails. `progress` receives short status updates for the UI.
pub fn list_experiences(
    cfg: &OAuthConfig,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<Experience>> {
    progress("Refreshing your Roblox session…");
    let (_session, token) = ensure_access_token(cfg)?;
    progress("Checking which experiences you granted access to…");
    let ids = granted_universe_ids(cfg, &token)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    progress(&format!("Loading {} experience{}…", ids.len(), if ids.len() == 1 { "" } else { "s" }));
    let mut exps = universe_details(&ids, &token)?;
    let thumbs = game_thumbnails(&ids);
    for e in &mut exps {
        if let Some(t) = thumbs.get(&e.universe_id) {
            e.thumbnail = t.clone();
        }
    }
    exps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(exps)
}

/// Downloads a place file (`.rbxl`, asset type 9) for editing. Uses the linked
/// Open Cloud API key (same path as Toolbox downloads) and requires the key's
/// account to own the experience / have `legacy-asset:manage` access.
pub fn download_place(place_id: u64) -> Result<AssetDownload> {
    let dl = download_asset(place_id as i64)?;
    let magic = &dl.bytes;
    if !(magic.starts_with(b"<roblox!") || magic.starts_with(b"<roblox")) {
        return Err(anyhow!(
            "the place download was not a Roblox place file (got {} bytes, type {})",
            dl.bytes.len(),
            dl.content_type
        ));
    }
    Ok(dl)
}

// ---------------------------------------------------------------------------
// Token storage (Windows Credential Manager, file fallback elsewhere)
// ---------------------------------------------------------------------------

const CRED_TARGET: &str = "RustbloxStudio/RobloxOAuth";
const OPENCLOUD_CRED_TARGET: &str = "RustbloxStudio/OpenCloud";

/// Persists the refresh token + identity in the OS credential store.
pub fn save_session(session: &Session) -> Result<()> {
    let blob = serde_json::to_vec(session).context("serializing session")?;
    creds::save(CRED_TARGET, &blob)
}

pub fn load_session() -> Result<Option<Session>> {
    let Some(blob) = creds::load(CRED_TARGET)? else {
        return Ok(None);
    };
    Ok(serde_json::from_slice(&blob).ok())
}

pub fn clear_session() -> Result<()> {
    creds::clear(CRED_TARGET)
}

// ---------------------------------------------------------------------------
// Open Cloud API key (same OS credential store, separate entry)
// ---------------------------------------------------------------------------

/// Stores an Open Cloud API key in the OS credential store. The key never
/// touches disk in plaintext on Windows (DPAPI-backed Credential Manager).
pub fn save_api_key(key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return Err(anyhow!("the API key is empty"));
    }
    if key.chars().any(|c| c.is_whitespace()) {
        return Err(anyhow!(
            "the API key contains whitespace — paste it again without line breaks"
        ));
    }
    creds::save(OPENCLOUD_CRED_TARGET, key.as_bytes())
}

pub fn load_api_key() -> Result<Option<String>> {
    let Some(bytes) = creds::load(OPENCLOUD_CRED_TARGET)? else {
        return Ok(None);
    };
    match String::from_utf8(bytes) {
        Ok(key) if !key.trim().is_empty() => Ok(Some(key)),
        _ => Ok(None),
    }
}

pub fn clear_api_key() -> Result<()> {
    creds::clear(OPENCLOUD_CRED_TARGET)
}

/// Display form of a key for the UI/logs: never the whole secret.
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.trim().chars().collect();
    if chars.len() <= 8 {
        return "••••••••".to_string();
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}••••••••{tail}")
}

// ---------------------------------------------------------------------------
// Asset download (Open Cloud `asset-delivery-api`, authenticated by API key)
// ---------------------------------------------------------------------------

/// Largest asset payload we will accept.
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;

/// A downloaded asset payload plus the metadata that came with it.
#[derive(Debug, Clone)]
pub struct AssetDownload {
    pub bytes: Vec<u8>,
    pub asset_type_id: Option<i64>,
    pub content_type: String,
}

/// Human label for Roblox asset type ids, used in insert errors.
pub fn asset_type_label(id: i64) -> String {
    let label = match id {
        1 => "Image",
        2 => "T-Shirt",
        3 => "Audio",
        4 => "Mesh",
        5 => "Lua script",
        8 => "Hat",
        9 => "Place",
        10 => "Model",
        11 => "Shirt",
        12 => "Pants",
        13 => "Decal",
        17 => "Head",
        18 => "Face",
        19 => "Gear",
        21 => "Badge",
        24 => "Animation",
        38 => "Plugin",
        40 => "Mesh",
        other => return format!("asset type {other}"),
    };
    label.to_string()
}

/// Downloads asset content. Prefers the Open Cloud asset-delivery API when an
/// API key is linked; otherwise falls back to the public asset endpoint (which
/// works for many publicly accessible assets such as material textures).
pub fn download_asset(asset_id: i64) -> Result<AssetDownload> {
    if let Some(key) = load_api_key()? {
        return download_asset_with_key(asset_id, &key);
    }
    download_public_asset(asset_id).map_err(|e| {
        anyhow!(
            "no Open Cloud API key is linked (Studio Settings -> Security & Accounts -> Link OpenCloud) and public download failed: {e:#}"
        )
    })
}

fn download_asset_with_key(asset_id: i64, key: &str) -> Result<AssetDownload> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let api_url = format!("https://apis.roblox.com/asset-delivery-api/v1/assetId/{asset_id}");
    let response = agent
        .get(&api_url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/json")
        .set("x-api-key", key)
        .call();
    let (status, body) = match response {
        Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => (code, resp.into_string().unwrap_or_default()),
        Err(e) => return Err(anyhow!("asset delivery request failed: {e}")),
    };
    match status {
        200 => {}
        401 => {
            return Err(anyhow!(
                "Open Cloud rejected the API key (401): the key is invalid, disabled, or this PC's IP is not on its allowlist"
            ))
        }
        403 => {
            return Err(anyhow!(
                "the API key lacks the Legacy Assets > Manage (legacy-asset:manage) scope (403)"
            ))
        }
        404 => return Err(anyhow!("asset {asset_id} does not exist (404)")),
        409 => {
            return Err(anyhow!(
                "this key's account is not entitled to asset {asset_id} (409): paid, private or other accounts' places cannot be downloaded - try your own assets or free public models"
            ))
        }
        other => {
            return Err(anyhow!(
                "asset delivery returned HTTP {other}: {}",
                truncate(&body, 200)
            ))
        }
    }

    let meta: Value =
        serde_json::from_str(&body).with_context(|| "parsing the asset delivery response")?;
    let asset_type_id = meta.get("assetTypeId").and_then(Value::as_i64);
    let location = meta
        .get("location")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the asset delivery response had no content location"))?;

    let mut bytes = Vec::new();
    let content_type;
    match agent
        .get(location)
        .set("User-Agent", USER_AGENT)
        .set("Accept-Encoding", "gzip")
        .call()
    {
        Ok(resp) => {
            content_type = resp
                .header("content-type")
                .unwrap_or("application/octet-stream")
                .to_string();
            let mut reader = resp.into_reader().take(MAX_ASSET_BYTES);
            reader
                .read_to_end(&mut bytes)
                .with_context(|| "reading the asset content")?;
        }
        Err(ureq::Error::Status(code, _)) => {
            return Err(anyhow!(
                "the asset CDN refused the download (HTTP {code}); the signed URL may have expired - try again"
            ))
        }
        Err(e) => return Err(anyhow!("downloading the asset content failed: {e}")),
    }
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut out)
            .with_context(|| "decompressing the asset content (gzip)")?;
        out
    } else {
        bytes
    };
    Ok(AssetDownload {
        bytes,
        asset_type_id,
        content_type,
    })
}

/// Best-effort public asset download (no Open Cloud key). Useful for assets such
/// as material textures that are commonly publicly accessible.
pub fn download_public_asset(asset_id: i64) -> Result<AssetDownload> {
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .redirects(5)
        .build();
    let url = format!("https://assetdelivery.roblox.com/v1/asset/?id={asset_id}");
    let resp = agent
        .get(&url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "*/*")
        .call();
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Err(anyhow!("asset {asset_id} was not found")),
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
            return Err(anyhow!("public download not permitted"));
        }
        Err(ureq::Error::Status(code, _)) => return Err(anyhow!("public download failed (HTTP {code})")),
        Err(e) => return Err(anyhow!("public asset request failed: {e}")),
    };
    let content_type = resp
        .header("content-type")
        .unwrap_or("application/octet-stream")
        .to_string();
    let mut bytes = Vec::new();
    resp.into_reader()
        .take((MAX_ASSET_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| anyhow!("reading asset {asset_id}: {e}"))?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(anyhow!("asset {asset_id} is too large ({} bytes)", bytes.len()));
    }
    if bytes.is_empty() {
        return Err(anyhow!("asset {asset_id} returned empty content"));
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut out)
            .map_err(|e| anyhow!("decompressing asset {asset_id} failed: {e}"))?;
        bytes = out;
    }
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_ascii_lowercase();
    if ct.starts_with("text/html") {
        return Err(anyhow!("asset endpoint returned HTML instead of content"));
    }
    Ok(AssetDownload {
        bytes,
        asset_type_id: None,
        content_type: ct,
    })
}

#[cfg(windows)]
mod creds {
    use anyhow::{anyhow, Result};
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(once(0)).collect()
    }

    pub fn save(target_name: &str, blob: &[u8]) -> Result<()> {
        let mut target = wide(target_name);
        let mut user = wide("rustblox");
        let mut comment = wide("Rustblox Studio credential");
        let mut blob = blob.to_vec();
        let credential = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: comment.as_mut_ptr(),
            LastWritten: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        if ok == 0 {
            return Err(anyhow!(
                "CredWriteW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub fn load(target_name: &str) -> Result<Option<Vec<u8>>> {
        let target = wide(target_name);
        let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
        if ok == 0 {
            // ERROR_NOT_FOUND (1168) means "no stored session", not a failure.
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(1168) {
                return Ok(None);
            }
            return Err(anyhow!("CredReadW failed: {err}"));
        }
        let bytes = unsafe {
            let cred = &*raw;
            let slice =
                std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
            let copy = slice.to_vec();
            CredFree(raw as *const core::ffi::c_void);
            copy
        };
        Ok(Some(bytes))
    }

    pub fn clear(target_name: &str) -> Result<()> {
        let target = wide(target_name);
        let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(1168) {
                return Ok(());
            }
            return Err(anyhow!("CredDeleteW failed: {err}"));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod creds {
    use anyhow::{Context, Result};
    use std::path::PathBuf;

    fn path(target_name: &str) -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(std::env::temp_dir);
        let file = target_name.replace(['/', '\\'], "-");
        base.join("rustblox-studio").join(format!("{file}.json"))
    }

    pub fn save(target_name: &str, blob: &[u8]) -> Result<()> {
        let path = path(target_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, blob).with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn load(target_name: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(path(target_name)) {
            Ok(blob) => Ok(Some(blob)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn clear(target_name: &str) -> Result<()> {
        match std::fs::remove_file(path(target_name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
