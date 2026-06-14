//! OCI registry credentials and configuration.
//!
//! This module provides:
//! - [`RegistryConfig`] — per-registry credential storage with env-var resolution
//! - Registry mirrors for pull-through caching
//!
//! Configuration is stored in `~/.config/smolvm/config.toml` under the
//! `[images]` section. See [`crate::settings::SmolSettings`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Registry credentials and defaults for a set of OCI registries.
///
/// Used as the `[images]` section within [`SmolSettings`](crate::settings::SmolSettings).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryConfig {
    /// Per-registry configuration entries.
    #[serde(default)]
    pub registries: HashMap<String, RegistryEntry>,
    /// Default settings.
    #[serde(default)]
    pub defaults: RegistryDefaults,
}

/// Configuration for a single registry.
///
/// Two distinct auth paths — only one should be set per entry:
///
/// **Identity token path** (`identity_token`): an upstream credential (e.g. Auth0 JWT)
/// exchanged with a token service per-operation to obtain a short-lived OCI bearer token.
/// Used for token-service-gated registries (e.g. Cloudflare-fronted OCI registries).
/// `RegistryClient::with_identity_token` implements the exchange.
///
/// **Direct bearer path** (`password` / `password_env`): an OCI bearer token sent
/// directly to the registry. Used for Docker Hub, GHCR, and other standard OCI
/// registries that accept static credentials.
///
/// `identity_token` takes precedence over `password`/`password_env` when both are set.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryEntry {
    /// Username for authentication (direct bearer path only).
    pub username: Option<String>,
    /// Direct OCI bearer token or password (not recommended for secrets; use password_env).
    pub password: Option<String>,
    /// Environment variable containing the direct OCI bearer token or password.
    pub password_env: Option<String>,
    /// Mirror URL to use instead of this registry.
    pub mirror: Option<String>,
    /// Upstream identity credential (e.g. Auth0 JWT) exchanged with a token service
    /// to obtain a short-lived OCI bearer token per-operation.
    /// When set, takes precedence over password/password_env.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_token: Option<String>,
    /// OAuth refresh token used to silently renew identity_token when it expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp when identity_token expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// Default registry settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryDefaults {
    /// Default registry when none specified (defaults to docker.io).
    pub registry: Option<String>,
}

// Re-export RegistryAuth from protocol to avoid duplication
pub use smolvm_protocol::RegistryAuth;

impl RegistryConfig {
    /// Get credentials for a registry, resolving environment variables.
    ///
    /// Returns `Some((username, password))` if credentials are configured and available.
    /// Returns `None` if:
    /// - No entry for this registry
    /// - No username configured
    /// - Password not available (env var not set, no direct password)
    pub fn get_credentials(&self, registry: &str) -> Option<RegistryAuth> {
        let entry = self.registries.get(registry)?;
        let username = entry.username.as_ref()?;

        // Try password_env first, then fall back to direct password
        let password = entry
            .password_env
            .as_ref()
            .and_then(|env| {
                std::env::var(env).ok().or_else(|| {
                    tracing::debug!(
                        registry = %registry,
                        env_var = %env,
                        "password environment variable not set"
                    );
                    None
                })
            })
            .or_else(|| {
                if entry.password.is_some() {
                    tracing::warn!(
                        registry = %registry,
                        "using plaintext password from config — use password_env instead"
                    );
                }
                entry.password.clone()
            })?;

        Some(RegistryAuth {
            username: username.clone(),
            password,
        })
    }

    /// Get mirror URL for a registry if configured.
    pub fn get_mirror(&self, registry: &str) -> Option<&str> {
        self.registries.get(registry)?.mirror.as_deref()
    }

    /// Get the default registry (defaults to "docker.io").
    pub fn default_registry(&self) -> &str {
        self.defaults
            .registry
            .as_deref()
            .unwrap_or(DEFAULT_REGISTRY)
    }

    /// Check if any registries are configured.
    pub fn has_registries(&self) -> bool {
        !self.registries.is_empty()
    }

    /// Set credentials for a registry, creating or updating the entry.
    ///
    /// Clears `password_env` when a direct password is provided. Preserves any
    /// existing `mirror` setting so callers do not need to re-supply it.
    pub fn set_credentials(&mut self, registry: &str, username: String, password: String) {
        let entry = self.registries.entry(registry.to_string()).or_default();
        entry.username = Some(username);
        entry.password = Some(password);
        entry.password_env = None;
        // Clear all upstream-credential fields. identity_token takes precedence
        // over password in build_registry_client(); leaving a stale identity_token
        // would silently ignore the new direct credentials.
        entry.identity_token = None;
        entry.refresh_token = None;
        entry.expires_at = None;
    }

    /// Set a token for a registry using the `username="token"` convention.
    ///
    /// Suitable for API keys and short-lived JWTs produced by `smol login`.
    /// Delegates to [`Self::set_credentials`] so mirror is preserved.
    pub fn set_token(&mut self, registry: &str, token: &str) {
        self.set_credentials(registry, "token".to_string(), token.to_string());
    }

    /// Store an upstream identity credential (e.g. an Auth0 JWT) for the
    /// registry. Unlike [`Self::set_token`], the credential is NOT sent to the
    /// registry directly — `build_registry_client` exchanges it at the
    /// registry's token service per operation (the OCI challenge flow), which
    /// is required for token-service-gated registries. Clears any
    /// direct-bearer fields so the exchange path always wins; preserves mirror.
    pub fn set_identity_token(&mut self, registry: &str, token: &str) {
        let entry = self.registries.entry(registry.to_string()).or_default();
        entry.identity_token = Some(token.to_string());
        entry.username = None;
        entry.password = None;
        entry.password_env = None;
    }
}

/// Default registry when none specified in image reference.
pub const DEFAULT_REGISTRY: &str = "docker.io";

/// Extract the registry hostname from an image reference.
///
/// # Examples
///
/// ```ignore
/// extract_registry("alpine") == "docker.io"
/// extract_registry("library/alpine") == "docker.io"
/// extract_registry("docker.io/library/alpine") == "docker.io"
/// extract_registry("ghcr.io/owner/repo") == "ghcr.io"
/// extract_registry("registry.example.com:5000/image") == "registry.example.com:5000"
/// ```
pub fn extract_registry(image: &str) -> String {
    // Check if the image starts with a registry (contains . or : before first /)
    if let Some(slash_pos) = image.find('/') {
        let potential_registry = &image[..slash_pos];

        // A registry hostname contains a dot (.) or a port (:)
        // This distinguishes "ghcr.io/owner/repo" from "library/alpine"
        if potential_registry.contains('.') || potential_registry.contains(':') {
            return potential_registry.to_string();
        }
    }

    // No explicit registry - use default
    DEFAULT_REGISTRY.to_string()
}

/// Rewrite an image reference to use a different registry.
///
/// # Examples
///
/// ```ignore
/// rewrite_image_registry("alpine", "mirror.example.com") == "mirror.example.com/library/alpine"
/// rewrite_image_registry("docker.io/library/alpine", "mirror.example.com") == "mirror.example.com/library/alpine"
/// rewrite_image_registry("ghcr.io/owner/repo", "mirror.example.com") == "mirror.example.com/owner/repo"
/// ```
pub fn rewrite_image_registry(image: &str, new_registry: &str) -> String {
    let current_registry = extract_registry(image);

    if image.starts_with(&current_registry) {
        // Explicit registry - replace it
        format!("{}{}", new_registry, &image[current_registry.len()..])
    } else {
        // Implicit docker.io - need to add "library/" for single-name images
        if image.contains('/') {
            format!("{}/{}", new_registry, image)
        } else {
            format!("{}/library/{}", new_registry, image)
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_registry_implicit_dockerhub() {
        assert_eq!(extract_registry("alpine"), "docker.io");
        assert_eq!(extract_registry("alpine:latest"), "docker.io");
        assert_eq!(extract_registry("library/alpine"), "docker.io");
        assert_eq!(extract_registry("myuser/myimage"), "docker.io");
    }

    #[test]
    fn test_extract_registry_explicit() {
        assert_eq!(extract_registry("docker.io/library/alpine"), "docker.io");
        assert_eq!(extract_registry("ghcr.io/owner/repo"), "ghcr.io");
        assert_eq!(extract_registry("gcr.io/project/image"), "gcr.io");
        assert_eq!(
            extract_registry("registry.example.com/image"),
            "registry.example.com"
        );
        assert_eq!(extract_registry("localhost:5000/image"), "localhost:5000");
    }

    #[test]
    fn test_rewrite_image_registry() {
        // Implicit docker.io
        assert_eq!(
            rewrite_image_registry("alpine", "mirror.example.com"),
            "mirror.example.com/library/alpine"
        );
        assert_eq!(
            rewrite_image_registry("myuser/myimage", "mirror.example.com"),
            "mirror.example.com/myuser/myimage"
        );

        // Explicit registry
        assert_eq!(
            rewrite_image_registry("docker.io/library/alpine", "mirror.example.com"),
            "mirror.example.com/library/alpine"
        );
        assert_eq!(
            rewrite_image_registry("ghcr.io/owner/repo", "mirror.example.com"),
            "mirror.example.com/owner/repo"
        );
    }

    #[test]
    fn test_registry_config_default() {
        let config = RegistryConfig::default();
        assert!(config.registries.is_empty());
        assert_eq!(config.default_registry(), "docker.io");
    }

    #[test]
    fn test_get_credentials_with_direct_password() {
        let mut config = RegistryConfig::default();
        config.registries.insert(
            "docker.io".to_string(),
            RegistryEntry {
                username: Some("testuser".to_string()),
                password: Some("testpass".to_string()),
                password_env: None,
                mirror: None,
                ..Default::default()
            },
        );

        let creds = config.get_credentials("docker.io");
        assert!(creds.is_some());
        let creds = creds.unwrap();
        assert_eq!(creds.username, "testuser");
        assert_eq!(creds.password, "testpass");
    }

    #[test]
    fn test_get_credentials_missing_username() {
        let mut config = RegistryConfig::default();
        config.registries.insert(
            "docker.io".to_string(),
            RegistryEntry {
                username: None,
                password: Some("testpass".to_string()),
                password_env: None,
                mirror: None,
                ..Default::default()
            },
        );

        assert!(config.get_credentials("docker.io").is_none());
    }

    #[test]
    fn test_get_credentials_missing_password() {
        let mut config = RegistryConfig::default();
        config.registries.insert(
            "docker.io".to_string(),
            RegistryEntry {
                username: Some("testuser".to_string()),
                password: None,
                password_env: None,
                mirror: None,
                ..Default::default()
            },
        );

        assert!(config.get_credentials("docker.io").is_none());
    }

    #[test]
    fn test_get_mirror() {
        let mut config = RegistryConfig::default();
        config.registries.insert(
            "docker.io".to_string(),
            RegistryEntry {
                username: None,
                password: None,
                password_env: None,
                mirror: Some("mirror.example.com".to_string()),
                ..Default::default()
            },
        );

        assert_eq!(config.get_mirror("docker.io"), Some("mirror.example.com"));
        assert_eq!(config.get_mirror("ghcr.io"), None);
    }

    #[test]
    fn test_parse_config() {
        let toml_content = r#"
[defaults]
registry = "docker.io"

[registries."docker.io"]
username = "myuser"
password_env = "DOCKER_TOKEN"

[registries."ghcr.io"]
username = "github_user"
password = "direct_password"
mirror = "ghcr-mirror.example.com"
"#;

        let config: RegistryConfig = toml::from_str(toml_content).unwrap();
        assert_eq!(config.registries.len(), 2);
        assert_eq!(config.default_registry(), "docker.io");

        let docker_entry = config.registries.get("docker.io").unwrap();
        assert_eq!(docker_entry.username.as_deref(), Some("myuser"));
        assert_eq!(docker_entry.password_env.as_deref(), Some("DOCKER_TOKEN"));

        let ghcr_entry = config.registries.get("ghcr.io").unwrap();
        assert_eq!(ghcr_entry.username.as_deref(), Some("github_user"));
        assert_eq!(ghcr_entry.password.as_deref(), Some("direct_password"));
        assert_eq!(
            ghcr_entry.mirror.as_deref(),
            Some("ghcr-mirror.example.com")
        );
    }

    #[test]
    fn test_get_credentials_with_env_password() {
        // Set environment variable for this test
        std::env::set_var("SMOLVM_TEST_TOKEN", "env_password_123");

        let mut config = RegistryConfig::default();
        config.registries.insert(
            "test.io".to_string(),
            RegistryEntry {
                username: Some("envuser".to_string()),
                password: None,
                password_env: Some("SMOLVM_TEST_TOKEN".to_string()),
                mirror: None,
                ..Default::default()
            },
        );

        let creds = config.get_credentials("test.io");
        assert!(creds.is_some());
        let creds = creds.unwrap();
        assert_eq!(creds.username, "envuser");
        assert_eq!(creds.password, "env_password_123");

        // Clean up
        std::env::remove_var("SMOLVM_TEST_TOKEN");
    }

    #[test]
    fn test_get_credentials_env_var_not_set() {
        let mut config = RegistryConfig::default();
        config.registries.insert(
            "test.io".to_string(),
            RegistryEntry {
                username: Some("user".to_string()),
                password: None,
                password_env: Some("SMOLVM_NONEXISTENT_VAR".to_string()),
                mirror: None,
                ..Default::default()
            },
        );

        // Should return None when env var is not set
        assert!(config.get_credentials("test.io").is_none());
    }

    #[test]
    fn test_has_registries() {
        let mut config = RegistryConfig::default();
        assert!(!config.has_registries());

        config
            .registries
            .insert("docker.io".to_string(), RegistryEntry::default());
        assert!(config.has_registries());
    }

    #[test]
    fn test_extract_registry_edge_cases() {
        // Image with tag containing colon (version)
        assert_eq!(extract_registry("alpine:3.18.0"), "docker.io");

        // Image with digest
        assert_eq!(extract_registry("alpine@sha256:abc123"), "docker.io");

        // Registry with port and path
        assert_eq!(
            extract_registry("registry.example.com:5000/myorg/myimage:latest"),
            "registry.example.com:5000"
        );
    }

    #[test]
    fn test_rewrite_image_registry_with_tag() {
        assert_eq!(
            rewrite_image_registry("alpine:3.18", "mirror.example.com"),
            "mirror.example.com/library/alpine:3.18"
        );

        assert_eq!(
            rewrite_image_registry("nginx:latest", "mirror.example.com"),
            "mirror.example.com/library/nginx:latest"
        );
    }

    #[test]
    fn test_default_registry_custom() {
        let mut config = RegistryConfig::default();
        config.defaults.registry = Some("custom.registry.io".to_string());
        assert_eq!(config.default_registry(), "custom.registry.io");
    }

    // ── set_credentials / set_token / save ──────────────────────────────────

    #[test]
    fn test_set_credentials_new_entry() {
        let mut config = RegistryConfig::default();
        config.set_credentials("registry.example.com", "user".into(), "pass".into());

        let creds = config.get_credentials("registry.example.com").unwrap();
        assert_eq!(creds.username, "user");
        assert_eq!(creds.password, "pass");
    }

    #[test]
    fn test_set_credentials_overwrites_password_env() {
        let mut config = RegistryConfig::default();
        config.registries.insert(
            "registry.example.com".to_string(),
            RegistryEntry {
                username: Some("old".to_string()),
                password: None,
                password_env: Some("OLD_TOKEN_ENV".to_string()),
                mirror: Some("mirror.example.com".to_string()),
                ..Default::default()
            },
        );

        config.set_credentials("registry.example.com", "new".into(), "newpass".into());

        let entry = config.registries.get("registry.example.com").unwrap();
        assert_eq!(entry.username.as_deref(), Some("new"));
        assert_eq!(entry.password.as_deref(), Some("newpass"));
        assert_eq!(entry.password_env, None, "password_env must be cleared");
        assert_eq!(
            entry.mirror.as_deref(),
            Some("mirror.example.com"),
            "mirror must be preserved"
        );
    }

    #[test]
    fn test_set_credentials_clears_identity_token() {
        // If an entry previously had an identity_token, switching to direct credentials
        // via set_credentials must clear it. identity_token takes precedence in
        // build_registry_client(); a stale one would silently ignore the new password.
        let mut config = RegistryConfig::default();
        config.registries.insert(
            "registry.example.com".to_string(),
            RegistryEntry {
                identity_token: Some("eyJ_old_jwt".to_string()),
                refresh_token: Some("old_refresh".to_string()),
                expires_at: Some(9999999999),
                ..Default::default()
            },
        );

        config.set_credentials(
            "registry.example.com",
            "user".into(),
            "direct_bearer".into(),
        );

        let entry = config.registries.get("registry.example.com").unwrap();
        assert_eq!(entry.password.as_deref(), Some("direct_bearer"));
        assert_eq!(
            entry.identity_token, None,
            "stale identity_token must be cleared"
        );
        assert_eq!(
            entry.refresh_token, None,
            "stale refresh_token must be cleared"
        );
        assert_eq!(entry.expires_at, None, "stale expires_at must be cleared");
    }

    #[test]
    fn test_set_token_uses_token_username() {
        let mut config = RegistryConfig::default();
        config.set_token("registry.example.com", "eyJhbGci.test");

        let creds = config.get_credentials("registry.example.com").unwrap();
        assert_eq!(creds.username, "token");
        assert_eq!(creds.password, "eyJhbGci.test");
    }

    #[test]
    fn test_save_roundtrip_preserves_all_fields() {
        let mut config = RegistryConfig::default();
        config.defaults.registry = Some("custom.io".to_string());
        config.registries.insert(
            "ghcr.io".to_string(),
            RegistryEntry {
                username: Some("gh_user".to_string()),
                password: Some("gh_pass".to_string()),
                password_env: None,
                mirror: Some("ghcr-mirror.example.com".to_string()),
                ..Default::default()
            },
        );

        let serialized = toml::to_string_pretty(&config).unwrap();
        let reloaded: RegistryConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(reloaded.default_registry(), "custom.io");
        let entry = reloaded.registries.get("ghcr.io").unwrap();
        assert_eq!(entry.username.as_deref(), Some("gh_user"));
        assert_eq!(entry.password.as_deref(), Some("gh_pass"));
        assert_eq!(entry.mirror.as_deref(), Some("ghcr-mirror.example.com"));
    }
}
