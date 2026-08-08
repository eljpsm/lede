//! The argument surface. Doc comments here are user-facing: clap prints
//! them as help text.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Generate a git commit message from the staged diff.
#[derive(Debug, Parser)]
#[command(name = "lede", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write the default configuration and system prompt, keeping any
    /// existing files, and check whether authentication resolves.
    Init,
    /// Generate a commit message and print it to stdout. Commit with
    /// `git commit -m "$(lede generate)"`.
    Generate {
        /// Extra context for the model, e.g. "refactor only".
        hint: Option<String>,

        /// Model to request, overriding the configuration.
        #[arg(short, long)]
        model: Option<String>,

        /// OpenAI-compatible API base URL, overriding the configuration.
        #[arg(long, value_name = "URL")]
        base_url: Option<String>,

        /// Read the system prompt from this file instead of the configured one.
        #[arg(long, value_name = "FILE")]
        system_prompt: Option<PathBuf>,
    },
}

// These pin the surface that shell aliases and scripts compose with. A
// parsing change that breaks one of them breaks `git commit -m
// "$(lede generate)"` invocations that already exist in users' configs.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_parses_and_takes_no_arguments() {
        assert!(matches!(
            Cli::try_parse_from(["lede", "init"]).unwrap().command,
            Command::Init
        ));
        assert!(Cli::try_parse_from(["lede", "init", "extra"]).is_err());
    }

    #[test]
    fn a_bare_generate_parses_with_nothing_set() {
        let cli = Cli::try_parse_from(["lede", "generate"]).unwrap();
        match cli.command {
            Command::Generate {
                hint,
                model,
                base_url,
                system_prompt,
            } => {
                assert!(hint.is_none());
                assert!(model.is_none());
                assert!(base_url.is_none());
                assert!(system_prompt.is_none());
            }
            other => panic!("expected generate, got {other:?}"),
        }
    }

    #[test]
    fn the_hint_is_generate_s_positional() {
        let cli = Cli::try_parse_from(["lede", "generate", "refactor only"]).unwrap();
        match cli.command {
            Command::Generate { hint, .. } => assert_eq!(hint.as_deref(), Some("refactor only")),
            other => panic!("expected generate, got {other:?}"),
        }
    }

    #[test]
    fn every_generate_override_parses() {
        let cli = Cli::try_parse_from([
            "lede",
            "generate",
            "-m",
            "qwen2.5-coder",
            "--base-url",
            "http://localhost:11434/v1",
            "--system-prompt",
            "/tmp/prompt.txt",
            "a hint",
        ])
        .unwrap();
        match cli.command {
            Command::Generate {
                hint,
                model,
                base_url,
                system_prompt,
            } => {
                assert_eq!(hint.as_deref(), Some("a hint"));
                assert_eq!(model.as_deref(), Some("qwen2.5-coder"));
                assert_eq!(base_url.as_deref(), Some("http://localhost:11434/v1"));
                assert_eq!(system_prompt, Some(PathBuf::from("/tmp/prompt.txt")));
            }
            other => panic!("expected generate, got {other:?}"),
        }
    }

    // A bare `lede` is an error, not a default command: the tool writes
    // files and spends API calls, so what it does must be explicit.
    #[test]
    fn no_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["lede"]).is_err());
    }

    #[test]
    fn an_unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["lede", "unknown"]).is_err());
    }

    // An unquoted hint (`lede generate fix the tests`) must fail loudly
    // rather than silently dropping every word after the first.
    #[test]
    fn a_second_positional_is_rejected() {
        assert!(Cli::try_parse_from(["lede", "generate", "one", "two"]).is_err());
    }
}
