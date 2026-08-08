//! Configuration: a TOML file and a system prompt file, both in
//! the config directory. First run writes both with defaults, so `lede` in
//! a fresh environment works as soon as the API key is set, and the system
//! prompt is always a file the user can open and edit rather than a string
//! buried in a binary.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::auth::AuthConfig;

pub(crate) const DEFAULT_CONFIG: &str = r#"# lede configuration

# OpenAI-compatible API base URL (without the trailing /chat/completions).
# Examples:
#   https://api.openai.com/v1
#   https://openrouter.ai/api/v1
#   https://api.groq.com/openai/v1
#   http://localhost:11434/v1   (Ollama)
base_url = "https://api.openai.com/v1"

# Model to request.
model = "gpt-5.4-nano"

# System prompt file, relative to this directory (absolute paths also work).
system_prompt_file = "prompt.txt"

[auth]
mode = "bearer"
# Name of the environment variable that holds the API key.
env = "OPENAI_API_KEY"
# Command run when the environment variable is not set.
# command = "pass show openai"

# For an endpoint that needs no authentication, replace the table above with:
# [auth]
# mode = "none"
"#;

pub(crate) const DEFAULT_PROMPT: &str = "\
You write git commit messages. You will receive the list of staged files
and the staged diff. Reply with the commit message and nothing else: no
markdown, no code fences, no quotes, no commentary.

Rules:
- Subject line: at most 50 characters, imperative mood (\"Add\", \"Fix\",
  \"Remove\"), capitalized, no trailing period.
- Describe the change itself, not the process (\"Fix null check\", never
  \"Fixed\" or \"This commit fixes\").
- Default to a subject only, even when several related files changed.
- Add a body only for a large change spanning distinct concerns, or when
  essential rationale or consequences cannot fit in the subject.
- A body must add information needed to understand the change. Never use it
  to list files, repeat the subject, or narrate the diff. Put it after one
  blank line, explain why rather than how, and wrap lines at 72 characters.
- Do not invent conventional-commit prefixes like \"feat:\" or \"fix:\";
  use one only if the project's changes clearly follow that style.
- Summarize the dominant change; do not enumerate every file.
";

/// config.toml as written. Field names are the TOML keys, and
/// `deny_unknown_fields` turns a misspelled key into a parse error rather
/// than a setting that is silently ignored.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub base_url: String,
    pub model: String,
    pub system_prompt_file: String,
    pub auth: AuthConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-5.4-nano".to_owned(),
            system_prompt_file: "prompt.txt".to_owned(),
            auth: AuthConfig::default(),
        }
    }
}

/// Everything `app` needs from disk, plus where it came from and which
/// files were written because they were missing, so `app` can report both
/// bootstrap ("wrote") and `init` idempotence ("kept existing").
#[derive(Debug)]
pub(crate) struct Loaded {
    pub config: Config,
    pub system_prompt: String,
    /// The config.toml actually read.
    pub config_file: PathBuf,
    /// The system prompt file actually read: the --system-prompt override,
    /// or the configured file resolved against the config dir.
    pub prompt_file: PathBuf,
    pub created: Vec<PathBuf>,
}

/// Load the config and the system prompt, bootstrapping defaults on first
/// run. `prompt_override` (the --system-prompt flag) is read as given and
/// never created: a missing override is the user's error to see.
pub(crate) fn load(prompt_override: Option<&Path>) -> anyhow::Result<Loaded> {
    load_from(&config_dir_from_env()?, prompt_override)
}

/// Split out from `load` so tests can point the config directory anywhere
/// without touching process-wide state.
fn load_from(dir: &Path, prompt_override: Option<&Path>) -> anyhow::Result<Loaded> {
    let mut created = Vec::new();

    let config_file = dir.join("config.toml");
    if !config_file.is_file() {
        write_default(dir, &config_file, DEFAULT_CONFIG)?;
        created.push(config_file.clone());
    }
    let text = std::fs::read_to_string(&config_file)
        .with_context(|| format!("failed to read {}", config_file.display()))?;
    let config =
        parse(&text).with_context(|| format!("invalid config {}", config_file.display()))?;

    let prompt_file = match prompt_override {
        Some(path) => path.to_path_buf(),
        None => {
            let path = Path::new(&config.system_prompt_file);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            }
        }
    };
    // The default prompt is recreated if deleted, but only inside the config
    // dir; a configured absolute path that is missing surfaces as an error.
    if prompt_override.is_none() && !prompt_file.is_file() && prompt_file.starts_with(dir) {
        write_default(dir, &prompt_file, DEFAULT_PROMPT)?;
        created.push(prompt_file.clone());
    }
    let system_prompt = std::fs::read_to_string(&prompt_file)
        .with_context(|| format!("failed to read system prompt {}", prompt_file.display()))?;

    Ok(Loaded {
        config,
        system_prompt,
        config_file,
        prompt_file,
        created,
    })
}

fn write_default(dir: &Path, file: &Path, contents: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    std::fs::write(file, contents).with_context(|| format!("failed to write {}", file.display()))
}

fn parse(text: &str) -> anyhow::Result<Config> {
    toml::from_str(text).map_err(anyhow::Error::from)
}

fn config_dir_from_env() -> anyhow::Result<PathBuf> {
    // The XDG spec says a relative XDG_CONFIG_HOME must be ignored.
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    config_dir(xdg, std::env::home_dir())
}

/// Split out from `config_dir_from_env` so the environment can be varied in
/// tests without touching process-wide state.
fn config_dir(xdg: Option<PathBuf>, home: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(xdg) = xdg {
        return Ok(xdg.join("lede"));
    }
    let home = home.context("cannot determine the home directory")?;
    Ok(home.join(".config/lede"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scratch config directory, removed on drop.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            // Tests run in parallel in one process, so the pid alone would
            // not keep two dirs apart.
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "lede-config-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
            ));
            ScratchDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, file_name: &str, contents: &str) {
            std::fs::create_dir_all(&self.0).unwrap();
            std::fs::write(self.0.join(file_name), contents).unwrap();
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_first_run_writes_config_and_prompt() {
        let dir = ScratchDir::new();

        let loaded = load_from(dir.path(), None).unwrap();

        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.system_prompt, DEFAULT_PROMPT);
        assert_eq!(
            loaded.created,
            vec![
                dir.path().join("config.toml"),
                dir.path().join("prompt.txt")
            ]
        );
    }

    #[test]
    fn the_default_prompt_makes_commit_bodies_exceptional() {
        assert!(DEFAULT_PROMPT.contains("Default to a subject only"));
        assert!(DEFAULT_PROMPT.contains("essential rationale or consequences"));
        assert!(DEFAULT_PROMPT.contains("Never use it\n  to list files"));
    }

    #[test]
    fn a_second_run_creates_nothing() {
        let dir = ScratchDir::new();
        load_from(dir.path(), None).unwrap();

        let loaded = load_from(dir.path(), None).unwrap();

        assert!(loaded.created.is_empty());
    }

    #[test]
    fn an_edited_config_is_honored() {
        let dir = ScratchDir::new();
        dir.write("config.toml", "model = \"llama3\"");

        let loaded = load_from(dir.path(), None).unwrap();

        assert_eq!(loaded.config.model, "llama3");
    }

    #[test]
    fn an_invalid_config_names_the_file() {
        let dir = ScratchDir::new();
        dir.write("config.toml", "modle = \"typo\"");

        let err = load_from(dir.path(), None).unwrap_err();

        assert!(format!("{err:#}").contains("invalid config"), "{err:#}");
    }

    #[test]
    fn a_deleted_prompt_is_recreated() {
        let dir = ScratchDir::new();
        load_from(dir.path(), None).unwrap();
        std::fs::remove_file(dir.path().join("prompt.txt")).unwrap();

        let loaded = load_from(dir.path(), None).unwrap();

        assert_eq!(loaded.created, vec![dir.path().join("prompt.txt")]);
        assert_eq!(loaded.system_prompt, DEFAULT_PROMPT);
    }

    #[test]
    fn an_absolute_prompt_path_is_read_as_given() {
        let dir = ScratchDir::new();
        dir.write("custom.txt", "custom prompt\n");
        let custom = dir.path().join("custom.txt");
        dir.write(
            "config.toml",
            &format!("system_prompt_file = \"{}\"", custom.display()),
        );

        let loaded = load_from(dir.path(), None).unwrap();

        assert_eq!(loaded.system_prompt, "custom prompt\n");
    }

    #[test]
    fn a_missing_absolute_prompt_path_is_not_created() {
        let dir = ScratchDir::new();
        dir.write(
            "config.toml",
            "system_prompt_file = \"/nonexistent/prompt.txt\"",
        );

        let err = load_from(dir.path(), None).unwrap_err();

        assert!(
            format!("{err:#}").contains("failed to read system prompt"),
            "{err:#}"
        );
        assert!(!Path::new("/nonexistent/prompt.txt").exists());
    }

    #[test]
    fn the_override_wins_over_the_configured_prompt() {
        let dir = ScratchDir::new();
        dir.write("override.txt", "override prompt\n");

        let loaded = load_from(dir.path(), Some(&dir.path().join("override.txt"))).unwrap();

        assert_eq!(loaded.system_prompt, "override prompt\n");
        // The configured prompt file is not bootstrapped when unused.
        assert_eq!(loaded.created, vec![dir.path().join("config.toml")]);
    }

    #[test]
    fn a_missing_override_is_an_error_not_a_bootstrap() {
        let dir = ScratchDir::new();
        let missing = dir.path().join("missing.txt");

        let err = load_from(dir.path(), Some(&missing)).unwrap_err();

        assert!(
            format!("{err:#}").contains("failed to read system prompt"),
            "{err:#}"
        );
        assert!(!missing.exists());
    }

    #[test]
    fn an_empty_config_yields_the_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    // Guards the template against drift: the file written on first run must
    // mean exactly what the built-in defaults mean.
    #[test]
    fn the_default_template_matches_the_defaults() {
        assert_eq!(parse(DEFAULT_CONFIG).unwrap(), Config::default());
    }

    #[test]
    fn a_partial_config_keeps_the_other_defaults() {
        let config = parse("model = \"llama3\"").unwrap();
        assert_eq!(config.model, "llama3");
        assert_eq!(config.base_url, Config::default().base_url);
        assert_eq!(config.auth, AuthConfig::default());
    }

    #[test]
    fn a_full_config_parses() {
        let config = parse(
            r#"
            base_url = "http://localhost:11434/v1"
            model = "qwen2.5-coder"
            system_prompt_file = "/etc/lede/prompt.txt"

            [auth]
            mode = "bearer"
            env = "OLLAMA_KEY"
            command = "pass show ollama"
            "#,
        )
        .unwrap();
        assert_eq!(config.base_url, "http://localhost:11434/v1");
        assert_eq!(config.model, "qwen2.5-coder");
        assert_eq!(config.system_prompt_file, "/etc/lede/prompt.txt");
        assert_eq!(
            config.auth,
            AuthConfig::Bearer {
                env: "OLLAMA_KEY".to_owned(),
                command: Some("pass show ollama".to_owned()),
            }
        );
    }

    #[test]
    fn no_authentication_parses() {
        let config = parse("[auth]\nmode = \"none\"").unwrap();
        assert_eq!(config.auth, AuthConfig::None {});
    }

    #[test]
    fn no_authentication_rejects_bearer_fields() {
        assert!(parse("[auth]\nmode = \"none\"\nenv = \"IGNORED\"").is_err());
    }

    #[test]
    fn bearer_authentication_defaults_its_environment() {
        let config = parse("[auth]\nmode = \"bearer\"").unwrap();
        assert_eq!(config.auth, AuthConfig::default());
    }

    // Authentication once lived in flat api_key_env / api_key_cmd keys. A
    // config written for that era must fail loudly so its owner migrates to
    // the [auth] table, not run on with the keys silently ignored.
    #[test]
    fn legacy_authentication_keys_are_rejected() {
        assert!(parse("api_key_env = \"OPENAI_API_KEY\"").is_err());
        assert!(parse("api_key_cmd = \"pass show openai\"").is_err());
    }

    #[test]
    fn a_misspelled_key_is_rejected() {
        assert!(parse("modle = \"typo\"").is_err());
    }

    #[test]
    fn xdg_config_home_overrides_home() {
        let dir = config_dir(
            Some(PathBuf::from("/custom/config")),
            Some(PathBuf::from("/home/user")),
        )
        .unwrap();
        assert_eq!(dir, PathBuf::from("/custom/config/lede"));
    }

    #[test]
    fn home_alone_yields_dot_config() {
        let dir = config_dir(None, Some(PathBuf::from("/home/user"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/user/.config/lede"));
    }

    #[test]
    fn no_home_at_all_is_an_error() {
        assert!(config_dir(None, None).is_err());
    }
}
