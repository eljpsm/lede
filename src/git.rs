//! Every invocation of the installed git. lede reads the staged state and
//! nothing else: what the user staged is the contract for what the message
//! describes, so an unstaged or untracked change never reaches the model.

use std::path::Path;
use std::process::Command;

use anyhow::Context;

/// Diffs beyond this are cut before the API call. About 16k tokens: enough
/// context for any commit worth one message, cheap enough to send blindly.
/// No token counting; bytes are a good enough proxy at this margin.
pub(crate) const MAX_DIFF_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct StagedChange {
    /// Name-status lines, e.g. "M\tsrc/main.rs". Never truncated, so the
    /// model sees the full scope of the commit even when the diff is cut.
    pub summary: String,
    /// The staged patch, truncated to `MAX_DIFF_BYTES`.
    pub diff: String,
}

fn run(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .context("failed to run git; is it installed?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One view of the staged change. `--raw --patch` emits the complete file
/// metadata and patch from one index read, so the two cannot describe
/// different staged states. `--no-ext-diff` guards against user diff tools
/// and `-M` collapses renames, so a moved file does not bloat the diff
/// with its whole contents.
///
/// The `split_once` leans on git's fixed output shape for this flag pair:
/// the raw entries, one blank line, then the patch.
pub(crate) fn staged_change(dir: &Path) -> anyhow::Result<StagedChange> {
    let out = run(
        dir,
        &[
            "diff",
            "--cached",
            "--raw",
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "-M",
        ],
    )?;
    if out.is_empty() {
        anyhow::bail!("nothing staged (use git add)");
    }
    let (raw, diff) = out
        .split_once("\n\n")
        .context("failed to parse staged diff")?;
    Ok(StagedChange {
        summary: raw_summary(raw)?,
        diff: truncate_diff(diff.to_owned(), MAX_DIFF_BYTES),
    })
}

/// Convert raw diff entries to the name-status lines sent to the model.
/// One entry looks like ":100644 100644 abc1234 def5678 M\tsrc/main.rs";
/// the status is the last metadata field and the paths keep their tabs, so
/// a rename ("R100\told\tnew") carries both of its paths through.
fn raw_summary(raw: &str) -> anyhow::Result<String> {
    raw.lines()
        .map(|line| {
            let (metadata, paths) = line
                .split_once('\t')
                .context("raw diff entry has no path")?;
            let status = metadata
                .split_whitespace()
                .next_back()
                .context("raw diff entry has no status")?;
            Ok(format!("{status}\t{paths}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

/// Cut at the last line boundary under `max`, never mid-codepoint, and say
/// so, so the model knows it is reading a prefix.
fn truncate_diff(diff: String, max: usize) -> String {
    if diff.len() <= max {
        return diff;
    }
    let mut end = max;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    let cut = diff[..end].rfind('\n').unwrap_or(end);
    format!(
        "{}\n\n[diff truncated: showing first {} KiB of {} KiB]",
        &diff[..cut],
        cut.div_ceil(1024),
        diff.len().div_ceil(1024),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scratch repository, removed on drop.
    struct ScratchRepo(PathBuf);

    impl ScratchRepo {
        fn new() -> Self {
            // Tests run in parallel in one process, so the pid alone would
            // not keep two repos apart.
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "lede-git-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&dir).unwrap();
            run(&dir, &["init", "-q"]).unwrap();
            ScratchRepo(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn stage_file(&self, name: &str, contents: &str) {
            fs::write(self.0.join(name), contents).unwrap();
            run(&self.0, &["add", name]).unwrap();
        }

        fn commit(&self) {
            run(
                &self.0,
                &[
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-q",
                    "-m",
                    "init",
                ],
            )
            .unwrap();
        }
    }

    impl Drop for ScratchRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_small_diff_is_left_alone() {
        assert_eq!(truncate_diff("small".into(), 100), "small");
        let exact = "x".repeat(100);
        assert_eq!(truncate_diff(exact.clone(), 100), exact);
    }

    #[test]
    fn truncation_cuts_at_a_line_boundary() {
        let diff = format!("{}\nline two is cut off here", "a".repeat(50));
        let out = truncate_diff(diff, 60);
        assert!(out.starts_with(&"a".repeat(50)));
        assert!(!out.contains("line two"));
        assert!(out.contains("[diff truncated"));
    }

    #[test]
    fn truncation_never_splits_a_codepoint() {
        let diff = "é".repeat(100); // 2 bytes each
        let out = truncate_diff(diff, 99);
        assert!(out.contains("[diff truncated"));
        assert!(out.starts_with(&"é".repeat(49)));
    }

    #[test]
    fn a_staged_change_yields_its_summary_and_diff() {
        let repo = ScratchRepo::new();
        repo.stage_file("hello.txt", "hello\n");
        repo.commit();
        repo.stage_file("hello.txt", "hello world\n");

        let change = staged_change(repo.path()).unwrap();
        assert_eq!(change.summary, "M\thello.txt");
        assert!(change.diff.contains("+hello world"));
        assert!(change.diff.contains("-hello"));
    }

    #[test]
    fn staged_files_before_the_first_commit_still_diff() {
        let repo = ScratchRepo::new();
        repo.stage_file("new.txt", "brand new\n");

        let change = staged_change(repo.path()).unwrap();
        assert_eq!(change.summary, "A\tnew.txt");
        assert!(change.diff.contains("+brand new"));
    }

    #[test]
    fn unstaged_changes_alone_are_nothing_staged() {
        let repo = ScratchRepo::new();
        repo.stage_file("a.txt", "a\n");
        repo.commit();
        fs::write(repo.path().join("a.txt"), "unstaged change\n").unwrap();

        let err = staged_change(repo.path()).unwrap_err();
        assert!(err.to_string().contains("nothing staged"));
    }

    #[test]
    fn a_rename_has_both_paths_in_its_summary() {
        let repo = ScratchRepo::new();
        repo.stage_file("old.txt", "same\n");
        repo.commit();
        fs::rename(repo.path().join("old.txt"), repo.path().join("new.txt")).unwrap();
        run(repo.path(), &["add", "-A"]).unwrap();

        let change = staged_change(repo.path()).unwrap();
        assert_eq!(change.summary, "R100\told.txt\tnew.txt");
    }

    #[test]
    fn a_unicode_path_is_kept_in_the_summary() {
        let repo = ScratchRepo::new();
        repo.stage_file("caf\u{e9}.txt", "coffee\n");

        let change = staged_change(repo.path()).unwrap();
        assert!(change.summary.starts_with("A\t"));
        assert!(change.summary.contains("caf"));
        assert!(change.diff.contains("+coffee"));
    }

    #[test]
    fn a_large_staged_patch_is_truncated() {
        let repo = ScratchRepo::new();
        repo.stage_file(
            "large.txt",
            &format!("{}\n", "x".repeat(MAX_DIFF_BYTES + 1024)),
        );

        let change = staged_change(repo.path()).unwrap();
        assert_eq!(change.summary, "A\tlarge.txt");
        assert!(change.diff.contains("[diff truncated"));
    }
}
