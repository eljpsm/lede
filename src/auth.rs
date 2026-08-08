//! Provider authentication. Configuration chooses the policy (the `[auth]`
//! table's `mode`); resolution reads a bearer token only when that policy
//! needs one. lede never stores a key: it reads the environment or shells
//! out to whatever secret manager the user configured, the way `git`
//! invokes the real git rather than reimplementing it.
//!
//! To add a mode, add an `AuthConfig` variant, a matching `Authorization`
//! carrier, an arm in `resolve`, and the header handling in `api::chat`.

use anyhow::Context;
use serde::Deserialize;

/// The `[auth]` table as written. `mode` selects the variant, and each
/// variant lists exactly the keys it accepts, so `deny_unknown_fields`
/// rejects a leftover `env` under `mode = "none"` instead of ignoring it.
/// `None {}` stays a struct variant for that reason: serde does not apply
/// the check to bare unit variants.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum AuthConfig {
    /// No credentials at all, for local endpoints like Ollama.
    None {},
    /// A bearer token: read from the named environment variable, or from
    /// the command's stdout when the variable is not set.
    Bearer {
        #[serde(default = "default_env")]
        env: String,
        #[serde(default)]
        command: Option<String>,
    },
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig::Bearer {
            env: default_env(),
            command: None,
        }
    }
}

/// What `api::chat` attaches to the request: nothing, or a resolved token
/// for an Authorization header. Separate from `AuthConfig` so the rest of
/// the program never sees where a credential came from.
pub(crate) enum Authorization {
    None,
    Bearer(String),
}

/// Turn the configured policy into request credentials. The one fallible
/// case is `Bearer`, which reads the environment here and leaves the rest
/// to `resolve_bearer`.
pub(crate) fn resolve(config: &AuthConfig) -> anyhow::Result<Authorization> {
    match config {
        AuthConfig::None {} => Ok(Authorization::None),
        AuthConfig::Bearer { env, command } => {
            resolve_bearer(env, command.as_deref(), std::env::var(env).ok())
                .map(Authorization::Bearer)
        }
    }
}

/// The environment wins over the command so a shell session can always
/// override a configured secret manager. Split out from `resolve` so tests
/// can vary the environment without touching process-wide state.
///
/// The command runs through `sh -c`, so arguments and pipes work without
/// lede doing any word splitting of its own. Its stdin is closed, but a
/// tool that prompts on the controlling terminal (a pinentry, a keyring
/// unlock) still can; lede waits for it, so that is not a hang.
fn resolve_bearer(
    env: &str,
    command: Option<&str>,
    env_value: Option<String>,
) -> anyhow::Result<String> {
    if let Some(key) = env_value.filter(|key| !key.trim().is_empty()) {
        return Ok(key);
    }
    let Some(command) = command.filter(|command| !command.trim().is_empty()) else {
        anyhow::bail!("environment variable {env} is not set (set it, or configure auth.command)");
    };
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("failed to run auth command {command:?}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("auth command {command:?} failed: {}", stderr.trim());
    }
    // Trimmed because nearly every secret tool prints a trailing newline,
    // and a newline inside a header value would break the request.
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        anyhow::bail!("auth command {command:?} printed nothing");
    }
    Ok(key)
}

fn default_env() -> String {
    "OPENAI_API_KEY".to_owned()
}

// Shell one-liners (`echo`, `printf`, `true`) stand in for the user's real
// secret manager: the contract under test is stdout, stderr, and exit
// status, which is all resolve_bearer ever sees of the real one.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_authentication_needs_no_credentials() {
        assert!(matches!(
            resolve(&AuthConfig::None {}),
            Ok(Authorization::None)
        ));
    }

    #[test]
    fn the_environment_wins_over_the_command() {
        let key = resolve_bearer(
            "OPENAI_API_KEY",
            Some("echo from-command"),
            Some("from-env".to_owned()),
        );
        assert_eq!(key.unwrap(), "from-env");
    }

    #[test]
    fn a_blank_environment_falls_through_to_the_command() {
        let key = resolve_bearer(
            "OPENAI_API_KEY",
            Some("echo from-command"),
            Some("  ".to_owned()),
        );
        assert_eq!(key.unwrap(), "from-command");
    }

    #[test]
    fn the_command_key_is_trimmed() {
        let key = resolve_bearer("OPENAI_API_KEY", Some("printf ' key \\n'"), None);
        assert_eq!(key.unwrap(), "key");
    }

    #[test]
    fn a_failing_command_reports_its_stderr() {
        let err = resolve_bearer(
            "OPENAI_API_KEY",
            Some("echo broken vault >&2; exit 1"),
            None,
        )
        .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains("auth command"), "{rendered}");
        assert!(rendered.contains("broken vault"), "{rendered}");
    }

    #[test]
    fn a_silent_command_is_an_error() {
        let err = resolve_bearer("OPENAI_API_KEY", Some("true"), None).unwrap_err();
        assert!(format!("{err:#}").contains("printed nothing"), "{err:#}");
    }

    #[test]
    fn neither_source_names_the_variable() {
        let err = resolve_bearer("OPENAI_API_KEY", None, None).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains("OPENAI_API_KEY is not set"), "{rendered}");
        assert!(rendered.contains("auth.command"), "{rendered}");
    }

    #[test]
    fn a_blank_command_counts_as_no_command() {
        let err = resolve_bearer("OPENAI_API_KEY", Some("  "), None).unwrap_err();
        assert!(
            format!("{err:#}").contains("OPENAI_API_KEY is not set"),
            "{err:#}"
        );
    }
}
