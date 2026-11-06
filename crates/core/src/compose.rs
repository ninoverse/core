//! Brings the stack declared in `containers/` up and down through the
//! `docker compose` CLI.
//!
//! Every `*.yaml` / `*.yml` file directly inside `containers/` is passed to
//! Compose as one project (repeated `-f`), so the files merge exactly as
//! `docker compose -f a.yaml -f b.yaml` would merge them. Parsing, ordering,
//! health gating and restart policy are Compose's job, not this module's.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
};

use thiserror::Error;
use tokio::{
    process::Command,
    sync::{broadcast::Sender, oneshot},
    task::JoinSet,
};

use logger::{error, info};

const COMPOSE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/containers");

/// Compose prefixes named volumes and networks with the project name, so it
/// has to stay stable across runs or existing data is orphaned.
const PROJECT_NAME: &str = "ninoverse";

/// Failures bringing the compose stack up or down.
#[derive(Debug, Error)]
pub enum ComposeError {
    /// The compose directory could not be read.
    #[error("Compose directory {path}: {source}")]
    Directory { path: String, source: io::Error },

    /// The compose directory holds no `*.yaml` or `*.yml` file.
    #[error("No compose files found in {0}")]
    NoComposeFiles(String),

    /// `docker compose` could not be started at all (CLI missing, not
    /// executable, ...).
    #[error("Failed to run `docker compose {subcommand}`: {source}")]
    Spawn {
        subcommand: &'static str,
        source: io::Error,
    },

    /// `docker compose` ran and failed. Its own diagnostics went to stderr.
    #[error("`docker compose {subcommand}` exited with {status}")]
    Failed {
        subcommand: &'static str,
        status: ExitStatus,
    },
}

/// Starts the compose stack and waits until Compose reports every service
/// running, healthy, or (for one-shot services) completed.
///
/// A task is added to `join_set` that tears the stack down once
/// `shutdown_sender` fires: `docker compose down` when
/// `remove_containers_on_shutdown` is set, `docker compose stop` otherwise.
/// The caller must broadcast shutdown before draining `join_set`, whatever
/// this function returns.
///
/// Returns `Ok(())` without the stack being up if shutdown is requested while
/// it is still starting.
pub async fn start_compose_stack(
    shutdown_sender: &Sender<()>,
    join_set: &mut JoinSet<()>,
    remove_containers_on_shutdown: bool,
) -> Result<(), ComposeError> {
    let compose_files = find_compose_files(Path::new(COMPOSE_DIR))?;
    info!(
        ["COMPOSE"],
        "Starting project '{}' from {} compose file(s)",
        PROJECT_NAME,
        compose_files.len()
    );

    let (up_result_sender, up_result_receiver) = oneshot::channel();
    let mut shutdown_receiver = shutdown_sender.subscribe();
    let teardown_subcommand = teardown_subcommand(remove_containers_on_shutdown);

    // `up` and the teardown share one task so the teardown can only start once
    // `up` has finished or been killed, never while it is still creating
    // containers.
    join_set.spawn(async move {
        tokio::select! {
            up_result = run_compose(&compose_files, &["up", "--detach", "--wait"]) => {
                let _ = up_result_sender.send(up_result);
                let _ = shutdown_receiver.recv().await;
            }
            _ = shutdown_receiver.recv() => {
                info!(["COMPOSE"], "Shutdown requested while the stack was starting.");
                // Release the caller now rather than after the teardown below.
                drop(up_result_sender);
            }
        }

        info!(
            ["COMPOSE"],
            "Running `docker compose {}`...", teardown_subcommand
        );
        if let Err(teardown_error) = run_compose(&compose_files, &[teardown_subcommand]).await {
            error!(["COMPOSE"], "{}", teardown_error);
        }
    });

    // The sender is dropped without sending only when shutdown won the race.
    up_result_receiver.await.unwrap_or(Ok(()))
}

fn teardown_subcommand(remove_containers_on_shutdown: bool) -> &'static str {
    if remove_containers_on_shutdown {
        "down"
    } else {
        "stop"
    }
}

/// Every `*.yaml` / `*.yml` file directly inside `compose_dir`, sorted so the
/// merge order Compose sees does not depend on the filesystem. Subdirectories
/// (`config/`, `standalone/`) are not descended into.
fn find_compose_files(compose_dir: &Path) -> Result<Vec<PathBuf>, ComposeError> {
    let directory_error = |source| ComposeError::Directory {
        path: compose_dir.display().to_string(),
        source,
    };

    let mut compose_files = Vec::new();
    for entry in fs::read_dir(compose_dir).map_err(directory_error)? {
        let path = entry.map_err(directory_error)?.path();
        let is_compose_file = path
            .extension()
            .is_some_and(|extension| extension == "yaml" || extension == "yml");
        if path.is_file() && is_compose_file {
            compose_files.push(path);
        }
    }

    if compose_files.is_empty() {
        return Err(ComposeError::NoComposeFiles(
            compose_dir.display().to_string(),
        ));
    }
    compose_files.sort();
    Ok(compose_files)
}

/// Arguments after `docker`: the project-wide options, then `subcommand_args`.
fn compose_args(compose_files: &[PathBuf], subcommand_args: &[&str]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "compose".into(),
        "--project-name".into(),
        PROJECT_NAME.into(),
        "--project-directory".into(),
        COMPOSE_DIR.into(),
    ];
    for compose_file in compose_files {
        args.push("--file".into());
        args.push(compose_file.into());
    }
    args.extend(subcommand_args.iter().map(OsString::from));
    args
}

async fn run_compose(
    compose_files: &[PathBuf],
    subcommand_args: &[&'static str],
) -> Result<(), ComposeError> {
    let subcommand = subcommand_args.first().copied().unwrap_or_default();

    let mut command = Command::new("docker");
    command
        .args(compose_args(compose_files, subcommand_args))
        .stdin(Stdio::null())
        // Dropping the future (shutdown during `up`) must not leave Compose
        // running behind our back.
        .kill_on_drop(true);
    // Keep Ctrl+C away from the child: shutdown is driven from here, and a
    // `docker compose up` interrupted by the terminal would race the shutdown
    // broadcast and surface as a startup failure.
    #[cfg(unix)]
    command.process_group(0);

    let status = command
        .status()
        .await
        .map_err(|source| ComposeError::Spawn { subcommand, source })?;

    if status.success() {
        Ok(())
    } else {
        Err(ComposeError::Failed { subcommand, status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty directory unique to `name` under the system temp dir.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("core-compose-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_only_top_level_yaml_files_in_sorted_order() {
        let dir = scratch_dir("sorted");
        for file in ["b.yaml", "a.yml", "c.yaml", "notes.txt", ".env"] {
            fs::write(dir.join(file), "").unwrap();
        }
        fs::create_dir(dir.join("standalone")).unwrap();
        fs::write(dir.join("standalone").join("kafka.yaml"), "").unwrap();
        fs::create_dir(dir.join("config.yaml")).unwrap();

        let found = find_compose_files(&dir).unwrap();

        assert_eq!(
            found,
            vec![dir.join("a.yml"), dir.join("b.yaml"), dir.join("c.yaml")]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn directory_without_compose_files_is_an_error() {
        let dir = scratch_dir("empty");
        fs::write(dir.join(".gitkeep"), "").unwrap();

        let result = find_compose_files(&dir);

        assert!(matches!(result, Err(ComposeError::NoComposeFiles(_))));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_directory_is_an_error() {
        let dir = scratch_dir("missing").join("absent");

        let result = find_compose_files(&dir);

        assert!(matches!(result, Err(ComposeError::Directory { .. })));
    }

    #[test]
    fn args_name_the_project_then_each_file_then_the_subcommand() {
        let files = [PathBuf::from("/c/a.yaml"), PathBuf::from("/c/b.yaml")];

        let args = compose_args(&files, &["up", "--detach", "--wait"]);

        let expected: Vec<OsString> = [
            "compose",
            "--project-name",
            PROJECT_NAME,
            "--project-directory",
            COMPOSE_DIR,
            "--file",
            "/c/a.yaml",
            "--file",
            "/c/b.yaml",
            "up",
            "--detach",
            "--wait",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        assert_eq!(args, expected);
    }

    #[test]
    fn teardown_removes_containers_only_when_configured() {
        assert_eq!(teardown_subcommand(true), "down");
        assert_eq!(teardown_subcommand(false), "stop");
    }
}
