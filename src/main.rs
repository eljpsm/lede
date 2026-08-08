//! lede generates a git commit message from staged changes.
//!
//! The pipeline: `cli` parses the subcommand and `app::run` dispatches it.
//! `git` captures the staged change, `config` loads the TOML and system
//! prompt (writing defaults on first run), `auth` resolves what the request
//! carries as credentials, `api` makes a chat-completions call, and
//! `format` holds the reply to the 50/72 rule.
//!
//! The finished message is the only thing on stdout. Diagnostics go to
//! stderr and failures exit nonzero, so `git commit -m "$(lede generate)"` composes
//! safely: a failed run aborts the commit instead of committing noise.

mod api;
mod app;
mod auth;
mod cli;
mod config;
mod format;
mod git;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    app::run(cli::Cli::parse())
}
