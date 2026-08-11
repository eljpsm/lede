//! Command dispatch and the generation pipeline: the only module that
//! prints and the only module that decides exit codes. Everything below
//! returns values or errors; how they reach the user is decided here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::api::{self, ChatMessage};
use crate::auth::{self, Authorization};
use crate::cli::{Cli, Command};
use crate::{config, format, git};

/// Dispatch one command. An `Err` reaching here prints once to stderr and
/// fails. On `generate`, the commit message is the only thing that ever
/// reaches stdout; `init` owns its stdout, since nothing composes with it.
pub(crate) fn run(cli: Cli) -> ExitCode {
    let result = match cli.command {
        Command::Init => init(),
        Command::Generate {
            hint,
            model,
            base_url,
            system_prompt,
        } => generate(hint, model, base_url, system_prompt).map(|message| println!("{message}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("lede: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Explicit first-run setup for people who want to edit the config before
/// the first generate spends an API call on the default endpoint.
/// Idempotent: existing files are kept, so it is safe to tell a user to
/// run it again. The authentication check is advisory and never fails the
/// command; the files were written, and the fix belongs to the user.
fn init() -> anyhow::Result<()> {
    let loaded = config::load(None)?;
    report_file(&loaded, &loaded.config_file);
    report_file(&loaded, &loaded.prompt_file);
    match auth::resolve(&loaded.config.auth) {
        Ok(Authorization::Bearer(_)) => println!("the api key resolves"),
        Ok(Authorization::None) => println!("no authentication configured"),
        Err(err) => println!("warning: {err:#}"),
    }
    Ok(())
}

fn report_file(loaded: &config::Loaded, file: &Path) {
    if loaded.created.contains(&file.to_path_buf()) {
        println!("wrote {}", file.display());
    } else {
        println!("kept existing {}", file.display());
    }
}

/// The pipeline itself. Order matters at the front: the staged change is
/// read before the config, so the most common failure (nothing staged) ends
/// the run before first-run bootstrap writes any files. The e2e tests pin
/// this by asserting the config dir stays absent on a doomed run.
fn generate(
    hint: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    system_prompt: Option<PathBuf>,
) -> anyhow::Result<String> {
    let change = git::staged_change(Path::new("."))?;

    let loaded = config::load(system_prompt.as_deref())?;
    for path in &loaded.created {
        eprintln!("lede: wrote {}", path.display());
    }
    let mut config = loaded.config;
    if let Some(model) = model {
        config.model = model;
    }
    if let Some(base_url) = base_url {
        config.base_url = base_url;
    }

    let authorization = auth::resolve(&config.auth)?;

    let mut messages = vec![
        ChatMessage {
            role: "system",
            content: loaded.system_prompt,
        },
        ChatMessage {
            role: "user",
            content: user_message(&change.summary, &change.diff, hint.as_deref()),
        },
    ];

    let agent = api::agent();
    let raw = api::chat(
        &agent,
        &config.base_url,
        &config.model,
        &authorization,
        &messages,
    )?;
    let mut message = format::parse(&format::sanitize(&raw));

    // The system prompt asks for a 50-character subject, but the reply is
    // not trusted: one retry asks the model to shorten, and a word-boundary
    // truncation is the last resort. Truncating beats erroring here, because
    // the user is already inside `git commit -m "$(lede generate)"` and a clipped
    // subject is still a valid, editable message.
    if !format::subject_fits(&message.subject) {
        messages.push(ChatMessage {
            role: "assistant",
            content: raw,
        });
        messages.push(ChatMessage {
            role: "user",
            content: format!(
                "The subject line is {} characters; the limit is {}. Rewrite the \
                 commit message with a shorter subject. Reply with only the commit message.",
                message.subject.chars().count(),
                format::SUBJECT_LIMIT,
            ),
        });
        if let Ok(retry) = api::chat(
            &agent,
            &config.base_url,
            &config.model,
            &authorization,
            &messages,
        ) {
            let retry = format::parse(&format::sanitize(&retry));
            if format::subject_fits(&retry.subject) {
                message = retry;
            }
        }
        if !format::subject_fits(&message.subject) {
            message.subject = format::truncate_subject(&message.subject);
        }
    }

    message.body = format::wrap_body(&message.body);
    Ok(format::render(&message))
}

/// The summary comes before the diff so the model sees the full scope of
/// the commit even when the diff is truncated.
fn user_message(summary: &str, diff: &str, hint: Option<&str>) -> String {
    let mut message = format!("Staged files:\n{summary}\n\nStaged diff:\n{diff}");
    if let Some(hint) = hint {
        message.push_str("\n\nAdditional context from the user: ");
        message.push_str(hint);
    }
    message
}

// The exact prompt layout is a contract with the default system prompt,
// which tells the model it will receive "the list of staged files and the
// staged diff". Renaming a section here means rewording the prompt too.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_message_holds_summary_then_diff() {
        let message = user_message("M\ta.rs", "+fn a() {}", None);
        assert_eq!(
            message,
            "Staged files:\nM\ta.rs\n\nStaged diff:\n+fn a() {}"
        );
    }

    #[test]
    fn a_hint_lands_at_the_end() {
        let message = user_message("M\ta.rs", "+x", Some("refactor only"));
        assert!(message.ends_with("Additional context from the user: refactor only"));
    }
}
