//! Load Docker client configuration and credentials from `~/.docker/config.json`.
//!
//! This crate reads the Docker CLI configuration file and resolves credentials
//! for registries, including support for external credential helpers
//! (`credsStore`, `credHelpers`) such as `osxkeychain`, `secretservice`, or `pass`.
//!
//! # Usage
//!
//! ## Loading credentials for a specific registry
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use docker_credentials_config::DockerConfig;
//!
//! let config = DockerConfig::load().await?;
//! if let Some(creds) = config.credentials_for_registry("https://index.docker.io/v1/") {
//!     println!("logged in as {:?}", creds.username);
//! }
//! # Ok(()) }
//! ```
//!
//! ## Resolving credentials from an image name
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use docker_credentials_config::{DockerConfig, image_registry};
//!
//! let config = DockerConfig::load().await?;
//! let registry = image_registry("gcr.io/myproject/myimage:latest");
//! let creds = config.credentials_for_registry(&registry);
//! # Ok(()) }
//! ```

use base64::{Engine, engine::general_purpose::STANDARD};
use bollard::auth::DockerCredentials;
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use error::Error;

mod error {
    /// Errors returned by this crate.
    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        /// The Docker configuration file exists but could not be read or parsed.
        #[error("Failed to parse Docker configuration file '{0}'")]
        DockerConfigParseError(String),
        /// The `auth` field in a registry entry is not valid base64.
        #[error("Invalid base64 in auth field: {0}")]
        AuthBase64Error(#[from] base64::DecodeError),
        /// The decoded `auth` field is not valid UTF-8.
        #[error("Auth field is not valid UTF-8: {0}")]
        AuthUtf8Error(#[from] std::string::FromUtf8Error),
    }
}

/// Default Docker configuration filename inside the config directory.
pub const DOCKER_CONFIG_FILENAME: &str = "config.json";

/// Canonical name for the Docker Hub registry.
pub const INDEX_NAME: &str = "docker.io";

/// Parsed Docker client configuration, loaded from `~/.docker/config.json`.
///
/// Contains inline credentials and references to external credential helpers.
/// Use [`DockerConfig::credentials_for_registry`] to resolve credentials for a
/// specific registry, or [`DockerConfig::all_credentials`] to collect credentials
/// for every known registry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DockerConfig {
    /// Inline credentials from the `auths` section of `config.json`, keyed
    /// by registry URL or hostname.
    #[serde(deserialize_with = "deserialize_auths")]
    pub auths: HashMap<String, DockerCredentials>,
    /// Global credential store helper name (e.g. `"osxkeychain"`).
    pub creds_store: Option<String>,
    /// Per-registry credential helpers, keyed by registry hostname.
    pub cred_helpers: HashMap<String, String>,
}

impl DockerConfig {
    /// Load the Docker configuration from the standard location.
    ///
    /// Search order:
    /// 1. `$DOCKER_CONFIG/config.json` (if `DOCKER_CONFIG` env var is set)
    /// 2. `~/.docker/config.json`
    ///
    /// Returns an empty config if no file is found.
    pub async fn load() -> Result<Self, Error> {
        let Some(path) = find_config_path() else {
            return Ok(Self::default());
        };
        Self::load_from_file(&path).await
    }

    pub(crate) async fn load_from_file(path: &Path) -> Result<Self, Error> {
        let contents = tokio::fs::read(path)
            .await
            .map_err(|_| Error::DockerConfigParseError(path.display().to_string()))?;
        serde_json::from_slice::<DockerConfig>(&contents)
            .map_err(|_| Error::DockerConfigParseError(path.display().to_string()))
    }

    /// Resolve credentials for the given registry, including via external
    /// credential helpers (`credsStore` / `credHelpers`).
    ///
    /// `registry` may be a hostname (`docker.io`, `gcr.io`) or a full URL
    /// (`https://index.docker.io/v1/`). The lookup order is:
    ///
    /// 1. Per-registry credential helper (`credHelpers`)
    /// 2. Inline `auth` or `identitytoken` in the `auths` section
    /// 3. Global credential store (`credsStore`)
    ///
    /// Returns `None` if no credentials are found or the credential helper
    /// fails (e.g. the helper binary is not installed).
    pub fn credentials_for_registry(&self, registry: &str) -> Option<DockerCredentials> {
        match docker_credential::get_credential(registry) {
            Ok(docker_credential::DockerCredential::UsernamePassword(username, password)) => {
                Some(DockerCredentials {
                    username: Some(username),
                    password: Some(password),
                    serveraddress: Some(registry.to_string()),
                    ..Default::default()
                })
            }
            Ok(docker_credential::DockerCredential::IdentityToken(token)) => {
                Some(DockerCredentials {
                    identitytoken: Some(token),
                    serveraddress: Some(registry.to_string()),
                    ..Default::default()
                })
            }
            Err(_) => None,
        }
    }

    /// Retrieve credentials for every registry that appears in the `auths`
    /// or `credHelpers` sections of the config file.
    ///
    /// Suitable for populating the `X-Registry-Config` header used by
    /// multi-registry operations such as `Docker::build_image`.
    ///
    /// Each registry is resolved via [`credentials_for_registry`](Self::credentials_for_registry),
    /// so credential helpers are invoked as needed. Registries whose helper
    /// fails or returns no credentials are silently omitted.
    pub fn all_credentials(&self) -> HashMap<String, DockerCredentials> {
        let mut result = HashMap::new();
        for registry in self.auths.keys().chain(self.cred_helpers.keys()) {
            if let Some(creds) = self.credentials_for_registry(registry) {
                result.insert(registry.clone(), creds);
            }
        }
        result
    }
}

/// Extract the registry hostname from a Docker image reference.
///
/// Follows the same rules as the Docker CLI:
/// - A bare name like `ubuntu` or `ubuntu:20.04` maps to `docker.io`.
/// - A path without a registry-like first component (e.g. `myuser/myimage`) maps to `docker.io`.
/// - A first component containing `.`, `:`, or equal to `localhost` is treated as the registry.
///
/// Digests (`@sha256:...`) are stripped before parsing.
///
/// # Examples
///
/// ```
/// use docker_credentials_config::image_registry;
///
/// assert_eq!(image_registry("ubuntu"), "docker.io");
/// assert_eq!(image_registry("gcr.io/myproject/myimage:latest"), "gcr.io");
/// assert_eq!(image_registry("localhost:5000/myimage"), "localhost:5000");
/// ```
pub fn image_registry(image: &str) -> String {
    // Strip digest (@sha256:...)
    let image = image.split('@').next().unwrap_or(image);
    // Split at first '/'
    let mut parts = image.splitn(2, '/');
    let first = parts.next().unwrap_or(image);
    // No '/' means it's a bare image name like "ubuntu:20.04"
    if parts.next().is_none() {
        return INDEX_NAME.to_string();
    }
    // First component is a registry if it contains '.', ':', or is "localhost"
    if first.contains('.') || first.contains(':') || first == "localhost" {
        first.to_string()
    } else {
        INDEX_NAME.to_string()
    }
}

fn find_config_path() -> Option<PathBuf> {
    // $DOCKER_CONFIG takes priority
    if let Ok(dir) = std::env::var("DOCKER_CONFIG") {
        let path = PathBuf::from(dir).join(DOCKER_CONFIG_FILENAME);
        if path.exists() {
            return Some(path);
        }
    }
    // Fall back to ~/.docker/config.json
    let base = directories::BaseDirs::new()?;
    let path = base.home_dir().join(".docker").join(DOCKER_CONFIG_FILENAME);
    if path.exists() { Some(path) } else { None }
}

/// Raw deserialization target for a single entry inside the `auths` map.
#[derive(Deserialize, Default)]
struct RawAuthEntry {
    /// Base64-encoded `"username:password"`.
    auth: Option<String>,
    email: Option<String>,
    identitytoken: Option<String>,
}

impl TryFrom<RawAuthEntry> for DockerCredentials {
    type Error = Error;

    fn try_from(entry: RawAuthEntry) -> Result<Self, Self::Error> {
        let mut creds = DockerCredentials {
            email: entry.email,
            identitytoken: entry.identitytoken,
            ..Default::default()
        };
        if let Some(auth_b64) = entry.auth {
            let decoded = STANDARD.decode(auth_b64.trim())?;
            let s = String::from_utf8(decoded)?;
            if let Some((user, pass)) = s.split_once(':') {
                creds.username = Some(user.to_string());
                creds.password = Some(pass.to_string());
            }
        }
        Ok(creds)
    }
}

fn deserialize_auths<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, DockerCredentials>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = HashMap::<String, RawAuthEntry>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|(registry, entry): (String, RawAuthEntry)| {
            let mut creds =
                DockerCredentials::try_from(entry).map_err(|e| serde::de::Error::custom(e))?;
            creds.serveraddress = Some(registry.clone());
            Ok((registry, creds))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DockerConfig, image_registry};
    use base64::Engine;

    // --- image_registry ---

    #[test]
    fn test_registry_bare_name() {
        assert_eq!(image_registry("ubuntu"), "docker.io");
    }

    #[test]
    fn test_registry_with_tag() {
        assert_eq!(image_registry("ubuntu:20.04"), "docker.io");
    }

    #[test]
    fn test_registry_official_path() {
        assert_eq!(image_registry("library/ubuntu"), "docker.io");
    }

    #[test]
    fn test_registry_user_path() {
        assert_eq!(image_registry("myuser/myimage:latest"), "docker.io");
    }

    #[test]
    fn test_registry_explicit_registry() {
        assert_eq!(image_registry("gcr.io/myproject/myimage:latest"), "gcr.io");
    }

    #[test]
    fn test_registry_localhost_with_port() {
        assert_eq!(
            image_registry("localhost:5000/myimage:latest"),
            "localhost:5000"
        );
    }

    #[test]
    fn test_registry_localhost_no_port() {
        assert_eq!(image_registry("localhost/myimage"), "localhost");
    }

    #[test]
    fn test_registry_with_digest() {
        assert_eq!(
            image_registry("gcr.io/myproject/myimage@sha256:abc123"),
            "gcr.io"
        );
    }

    #[test]
    fn test_registry_bare_digest() {
        assert_eq!(image_registry("ubuntu@sha256:abc123"), "docker.io");
    }

    // --- DockerConfig::load_from_file ---

    #[tokio::test]
    async fn test_load_from_file_parses_auths() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"{{
                "auths": {{
                    "https://index.docker.io/v1/": {{
                        "auth": "{}"
                    }}
                }}
            }}"#,
            base64::engine::general_purpose::STANDARD.encode("alice:password123")
        )
        .unwrap();
        let config = DockerConfig::load_from_file(tmp.path()).await.unwrap();
        let creds = config.auths.get("https://index.docker.io/v1/").unwrap();
        assert_eq!(creds.username.as_deref(), Some("alice"));
        assert_eq!(creds.password.as_deref(), Some("password123"));
    }

    #[tokio::test]
    async fn test_load_from_file_returns_error_on_invalid_json() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "not valid json").unwrap();
        let result = DockerConfig::load_from_file(tmp.path()).await;
        assert!(matches!(
            result,
            Err(crate::Error::DockerConfigParseError(_))
        ));
    }

    #[tokio::test]
    async fn test_load_from_file_returns_error_on_missing_file() {
        let result = DockerConfig::load_from_file(std::path::Path::new(
            "/tmp/docker_credentials_config_test_nonexistent.json",
        ))
        .await;
        assert!(matches!(
            result,
            Err(crate::Error::DockerConfigParseError(_))
        ));
    }

    #[tokio::test]
    async fn test_load_from_file_empty_config() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{{}}").unwrap();
        let config = DockerConfig::load_from_file(tmp.path()).await.unwrap();
        assert!(config.auths.is_empty());
        assert!(config.creds_store.is_none());
        assert!(config.cred_helpers.is_empty());
    }
}
