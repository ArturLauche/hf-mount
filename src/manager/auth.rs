//! Hugging Face credentials: where a token comes from, validating it, and
//! keeping it out of logs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::protocol::{Credential, ErrorCode, ErrorInfo};

/// A token that never prints itself and is wiped from memory on drop.
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let v = value.into().trim().to_string();
        (!v.is_empty()).then_some(Self(v))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: overwriting initialised bytes with zeros keeps valid UTF-8.
        unsafe {
            for b in self.0.as_bytes_mut() {
                std::ptr::write_volatile(b, 0);
            }
        }
    }
}

/// Where a token was found. Reported in status output; never the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenSource {
    TokenFile { path: PathBuf },
    CommandLine,
    Environment { variable: String },
    CredentialStore,
    HuggingFaceCli { path: PathBuf },
}

impl TokenSource {
    pub fn label(&self) -> String {
        match self {
            TokenSource::TokenFile { path } => format!("token file {}", path.display()),
            TokenSource::CommandLine => "--hf-token argument".to_string(),
            TokenSource::Environment { variable } => format!("{variable} environment variable"),
            TokenSource::CredentialStore => "Windows Credential Manager".to_string(),
            TokenSource::HuggingFaceCli { path } => format!("huggingface_hub token ({})", path.display()),
        }
    }
}

/// Location of the token saved by `hf auth login` / `huggingface-cli login`.
pub fn hf_cli_token_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HF_TOKEN_PATH").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HF_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join("token"));
    }
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(".cache").join("huggingface").join("token"))
}

fn read_token_file(path: &Path) -> Option<Secret> {
    std::fs::read_to_string(path).ok().and_then(Secret::new)
}

/// Inputs to [`resolve_token`]; each source can be disabled.
pub struct TokenSources<'a> {
    pub token_file: Option<&'a Path>,
    pub command_line: Option<&'a str>,
    pub use_environment: bool,
    /// Reads the OS credential store (Windows Credential Manager).
    pub credential_store: Option<&'a dyn Fn() -> Option<Secret>>,
    pub use_hf_cli: bool,
}

/// The credential to send for a mount, and where it came from.
///
/// Order: `--token-file`, `--hf-token`, `HF_TOKEN`, the OS credential store,
/// then the huggingface_hub CLI token. `None` means anonymous access, which
/// works for public repositories.
pub fn resolve_token(sources: &TokenSources<'_>) -> Option<(Credential, TokenSource)> {
    if let Some(path) = sources.token_file {
        // The file is re-read by the engine before Hub requests, so pass the
        // path (rotation keeps working) rather than its current content.
        return Some((
            Credential::TokenFile {
                path: path.to_path_buf(),
            },
            TokenSource::TokenFile {
                path: path.to_path_buf(),
            },
        ));
    }
    let token = |secret: Secret, source: TokenSource| {
        Some((
            Credential::Token {
                token: secret.expose().to_string(),
                source: source.label(),
            },
            source,
        ))
    };
    if let Some(secret) = sources.command_line.and_then(Secret::new) {
        return token(secret, TokenSource::CommandLine);
    }
    if sources.use_environment {
        for variable in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"] {
            if let Some(secret) = std::env::var(variable).ok().and_then(Secret::new) {
                return token(
                    secret,
                    TokenSource::Environment {
                        variable: variable.to_string(),
                    },
                );
            }
        }
    }
    if let Some(store) = sources.credential_store
        && let Some(secret) = store()
    {
        return token(secret, TokenSource::CredentialStore);
    }
    if sources.use_hf_cli
        && let Some(path) = hf_cli_token_path()
        && let Some(secret) = read_token_file(&path)
    {
        return token(secret, TokenSource::HuggingFaceCli { path });
    }
    None
}

/// The account a token belongs to, from `/api/whoami-v2`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Display name of the access token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_name: Option<String>,
    /// `read`, `write` or `fineGrained`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_role: Option<String>,
    #[serde(default)]
    pub orgs: Vec<String>,
}

impl Account {
    /// Parse the `/api/whoami-v2` response body.
    pub fn from_whoami(body: &serde_json::Value) -> Option<Self> {
        let name = body.get("name")?.as_str()?.to_string();
        let access = body.get("auth").and_then(|a| a.get("accessToken"));
        Some(Self {
            name,
            fullname: body.get("fullname").and_then(|v| v.as_str()).map(str::to_string),
            avatar_url: body.get("avatarUrl").and_then(|v| v.as_str()).map(str::to_string),
            token_name: access
                .and_then(|a| a.get("displayName"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            token_role: access
                .and_then(|a| a.get("role"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            orgs: body
                .get("orgs")
                .and_then(|v| v.as_array())
                .map(|orgs| {
                    orgs.iter()
                        .filter_map(|o| o.get("name").and_then(|n| n.as_str()).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Whether the token can be expected to write to buckets.
    pub fn can_write(&self) -> bool {
        matches!(self.token_role.as_deref(), Some("write") | Some("fineGrained"))
    }
}

pub const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// Validate `token` against the Hub and return its account.
pub async fn whoami(endpoint: &str, token: &str) -> Result<Account, ErrorInfo> {
    let client = reqwest::Client::builder()
        .user_agent(format!("hf-mount/{}; whoami", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("HTTP client: {e}")))?;
    let url = format!("{}/api/whoami-v2", endpoint.trim_end_matches('/'));
    let resp = client.get(&url).bearer_auth(token).send().await.map_err(|e| {
        ErrorInfo::new(ErrorCode::Network, format!("could not reach {endpoint}: {e}"))
            .with_hint("check your internet connection or proxy settings")
    })?;
    match resp.status().as_u16() {
        200 => {
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| ErrorInfo::new(ErrorCode::Internal, format!("unexpected whoami response: {e}")))?;
            Account::from_whoami(&body)
                .ok_or_else(|| ErrorInfo::new(ErrorCode::Internal, "unexpected whoami response (no account name)"))
        }
        401 => Err(
            ErrorInfo::new(ErrorCode::InvalidToken, "the Hugging Face token is invalid or expired")
                .with_hint("create a new token at https://huggingface.co/settings/tokens"),
        ),
        429 => Err(ErrorInfo::new(
            ErrorCode::RateLimited,
            "the Hub is rate limiting requests; try again shortly",
        )),
        status => Err(ErrorInfo::new(
            ErrorCode::Network,
            format!("the Hub answered HTTP {status} while validating the token"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_debug_prints() {
        let s = Secret::new("  hf_abcdefghijkl  ").unwrap();
        assert_eq!(s.expose(), "hf_abcdefghijkl");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert!(Secret::new("   ").is_none());
    }

    #[test]
    fn token_file_wins_and_is_passed_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "hf_fromfile1234").unwrap();
        let (cred, source) = resolve_token(&TokenSources {
            token_file: Some(&path),
            command_line: Some("hf_cli_value_123"),
            use_environment: false,
            credential_store: None,
            use_hf_cli: false,
        })
        .unwrap();
        assert_eq!(cred, Credential::TokenFile { path: path.clone() });
        assert_eq!(source, TokenSource::TokenFile { path });
    }

    #[test]
    fn command_line_then_store_then_none() {
        let store = || Secret::new("hf_from_store_123");
        let (cred, source) = resolve_token(&TokenSources {
            token_file: None,
            command_line: Some("hf_cli_value_123"),
            use_environment: false,
            credential_store: Some(&store),
            use_hf_cli: false,
        })
        .unwrap();
        assert_eq!(source, TokenSource::CommandLine);
        assert!(matches!(cred, Credential::Token { ref token, .. } if token == "hf_cli_value_123"));

        let (cred, source) = resolve_token(&TokenSources {
            token_file: None,
            command_line: None,
            use_environment: false,
            credential_store: Some(&store),
            use_hf_cli: false,
        })
        .unwrap();
        assert_eq!(source, TokenSource::CredentialStore);
        assert!(matches!(cred, Credential::Token { ref token, .. } if token == "hf_from_store_123"));

        assert!(
            resolve_token(&TokenSources {
                token_file: None,
                command_line: Some("   "),
                use_environment: false,
                credential_store: None,
                use_hf_cli: false,
            })
            .is_none()
        );
    }

    #[test]
    fn parses_whoami_body() {
        let body = serde_json::json!({
            "type": "user",
            "name": "alice",
            "fullname": "Alice A.",
            "avatarUrl": "https://cdn/avatar.png",
            "orgs": [{"name": "acme"}, {"name": "hf"}],
            "auth": {"type": "access_token", "accessToken": {"displayName": "laptop", "role": "write"}}
        });
        let account = Account::from_whoami(&body).unwrap();
        assert_eq!(account.name, "alice");
        assert_eq!(account.token_name.as_deref(), Some("laptop"));
        assert!(account.can_write());
        assert_eq!(account.orgs, vec!["acme", "hf"]);
        let read_only = serde_json::json!({"name": "bob", "auth": {"accessToken": {"role": "read"}}});
        assert!(!Account::from_whoami(&read_only).unwrap().can_write());
        assert!(Account::from_whoami(&serde_json::json!({})).is_none());
    }
}
