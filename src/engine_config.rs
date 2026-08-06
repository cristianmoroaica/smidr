//! `~/.config/smidr/engines.toml` — local OpenAI-compatible endpoint config.
//!
//! Loaded fresh on every call (no caching) so it can be edited without a
//! restart, and so tests can point it at a tempdir via `XDG_CONFIG_HOME`.
//!
//! Consumers: `AppCore::set_engine`/`apply_engine` (resolving a persisted
//! `"<endpoint>:<model>"` id), `GET /api/engines` (listing + model
//! discovery), and `startup_checks` (the no-engines-at-all hint).

use serde::Deserialize;

/// The wire protocol an endpoint speaks.
#[derive(Debug, Clone, PartialEq)]
pub enum EndpointKind {
    /// Ollama's OpenAI-compatible `/v1` surface, plus native `/api/tags` for
    /// model discovery.
    Ollama,
    /// A generic OpenAI-compatible endpoint (llama.cpp, LM Studio, vLLM…).
    OpenAi,
}

/// One configured local-model endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointConfig {
    pub name: String,
    pub kind: EndpointKind,
    pub base_url: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(default)]
    endpoint: Vec<RawEndpoint>,
}

#[derive(Debug, Deserialize)]
struct RawEndpoint {
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    api_key: Option<String>,
}

/// Path to `engines.toml`: `$XDG_CONFIG_HOME/smidr/engines.toml` when
/// `XDG_CONFIG_HOME` is set and non-empty, else `$HOME/.config/smidr/engines.toml`.
/// Re-reads the environment on every call — no caching — so tests can point
/// it at a tempdir.
pub fn engines_toml_path() -> std::path::PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return std::path::PathBuf::from(xdg).join("smidr/engines.toml");
        }
    }
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_default();
    home.join(".config/smidr/engines.toml")
}

/// Load and validate all configured endpoints. Missing file -> empty vec,
/// silently. Unreadable/malformed file -> empty vec + one warning. Invalid
/// individual entries are skipped with one warning each; the rest still load.
pub fn load_endpoints() -> Vec<EndpointConfig> {
    let path = engines_toml_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return vec![],
        Err(e) => {
            eprintln!("Warning: could not read {}: {e}", path.display());
            return vec![];
        }
    };
    let raw: RawFile = match toml::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: could not parse {}: {e}", path.display());
            return vec![];
        }
    };

    let mut out = Vec::new();
    for ep in raw.endpoint {
        if ep.name.is_empty() {
            eprintln!("Warning: engines.toml entry skipped: empty name");
            continue;
        }
        if ep.name.contains(':') {
            eprintln!(
                "Warning: engines.toml entry \"{}\" skipped: name must not contain ':'",
                ep.name
            );
            continue;
        }
        if ep.base_url.is_empty() {
            eprintln!(
                "Warning: engines.toml entry \"{}\" skipped: empty base_url",
                ep.name
            );
            continue;
        }
        let kind = match ep.kind.to_lowercase().as_str() {
            "ollama" => EndpointKind::Ollama,
            "openai" => EndpointKind::OpenAi,
            other => {
                eprintln!(
                    "Warning: engines.toml entry \"{}\" skipped: unknown type \"{other}\"",
                    ep.name
                );
                continue;
            }
        };
        let base_url = ep.base_url.trim_end_matches('/').to_string();
        out.push(EndpointConfig {
            name: ep.name,
            kind,
            base_url,
            api_key: ep.api_key,
        });
    }
    out
}

/// Find a configured endpoint by name.
pub fn find_endpoint(name: &str) -> Option<EndpointConfig> {
    load_endpoints().into_iter().find(|e| e.name == name)
}

/// Split a persisted engine id on the FIRST colon only, so model names that
/// themselves contain colons (e.g. `gpt-oss:120b`) round-trip correctly.
/// `"claude"` -> `None`. An empty model half (e.g. `"x:"`) is also `None` —
/// a persisted engine id always names a real model, so a bare trailing colon
/// is treated as not naming a local engine at all rather than a local engine
/// with an empty model.
pub fn split_engine_id(id: &str) -> Option<(String, String)> {
    let (endpoint, model) = id.split_once(':')?;
    if endpoint.is_empty() || model.is_empty() {
        return None;
    }
    Some((endpoint.to_string(), model.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_LOCK;
    use tempfile::TempDir;

    /// Point `XDG_CONFIG_HOME` at a fresh tempdir, write `files` under
    /// `smidr/`, run `f`, then restore both `HOME` and `XDG_CONFIG_HOME`.
    /// Mirrors `with_test_root` in src/storage/project.rs.
    fn with_config_home(files: &[(&str, &str)], f: impl FnOnce()) {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let smidr_dir = tmp.path().join("smidr");
        std::fs::create_dir_all(&smidr_dir).unwrap();
        for (name, contents) in files {
            std::fs::write(smidr_dir.join(name), contents).unwrap();
        }

        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        std::env::set_var("HOME", tmp.path());

        f();

        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn valid_two_endpoint_file_parses() {
        with_config_home(
            &[(
                "engines.toml",
                r#"
[[endpoint]]
name = "ollama"
type = "ollama"
base_url = "http://127.0.0.1:11434/v1/"

[[endpoint]]
name = "llamacpp"
type = "openai"
base_url = "http://127.0.0.1:8080/v1"
api_key = "secret"
"#,
            )],
            || {
                let endpoints = load_endpoints();
                assert_eq!(endpoints.len(), 2);
                assert_eq!(
                    endpoints[0],
                    EndpointConfig {
                        name: "ollama".to_string(),
                        kind: EndpointKind::Ollama,
                        base_url: "http://127.0.0.1:11434/v1".to_string(),
                        api_key: None,
                    }
                );
                assert_eq!(
                    endpoints[1],
                    EndpointConfig {
                        name: "llamacpp".to_string(),
                        kind: EndpointKind::OpenAi,
                        base_url: "http://127.0.0.1:8080/v1".to_string(),
                        api_key: Some("secret".to_string()),
                    }
                );
                assert_eq!(find_endpoint("llamacpp").unwrap().base_url, "http://127.0.0.1:8080/v1");
                assert!(find_endpoint("nope").is_none());
            },
        );
    }

    #[test]
    fn missing_file_returns_empty_vec() {
        with_config_home(&[], || {
            assert_eq!(load_endpoints(), vec![]);
        });
    }

    #[test]
    fn garbage_toml_returns_empty_vec() {
        with_config_home(&[("engines.toml", "this is not valid toml {{{")], || {
            assert_eq!(load_endpoints(), vec![]);
        });
    }

    #[test]
    fn endpoint_name_with_colon_is_skipped_sibling_kept() {
        with_config_home(
            &[(
                "engines.toml",
                r#"
[[endpoint]]
name = "bad:name"
type = "ollama"
base_url = "http://127.0.0.1:11434/v1"

[[endpoint]]
name = "good"
type = "ollama"
base_url = "http://127.0.0.1:11434/v1"
"#,
            )],
            || {
                let endpoints = load_endpoints();
                assert_eq!(endpoints.len(), 1);
                assert_eq!(endpoints[0].name, "good");
            },
        );
    }

    #[test]
    fn split_engine_id_cases() {
        assert_eq!(split_engine_id("claude"), None);
        assert_eq!(
            split_engine_id("ollama:gpt-oss:120b"),
            Some(("ollama".to_string(), "gpt-oss:120b".to_string()))
        );
        assert_eq!(split_engine_id("x:"), None);
    }
}
