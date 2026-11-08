// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Running `prepare`, `run`, and `smoke` on another host, such as a dedicated measuring machine, over SSH.
//!
//! Every local path a command names is mirrored on the host at `DIRECTORY/mirror/ABSOLUTE-PATH`, so artifacts and campaigns prepared there refer to each other as they would locally. Paths are made canonical locally first, so a mirrored path never leaves the mirror. Before the command, the workspace, each tree, and the trees' path dependencies outside them are copied there with `rsync`, without their `target` directories but with their Git state, including the repositories of linked worktrees. Afterwards the command's output directory is copied back, also when the command failed or was cancelled, so such campaigns can be inspected; a command that failed before creating it has nothing to copy, and its own error is reported alone. Downloaded artifact manifests and campaigns name the host's paths: run prepared artifacts on the same host, and summarize campaigns anywhere.

use crate::config::{invalid_input, program};
use crate::prepare::external_path_packages;
use crate::runner::Cancellation;
use serde_json::Value;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Where and how to run commands remotely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// The SSH destination, such as `bench.local` or `user@host`.
    pub host: String,
    /// The host's directory for mirrors and the installed command. A relative path is under the remote user's home directory. Mirrors are keyed by local paths only, so machines or users sharing a host account need directories of their own.
    pub directory: String,
    /// The `base-benchmark` command on the host, as a path there. Without it, the release of this version is installed from crates.io under `DIRECTORY/tool` when it is missing, so an unreleased build needs a program of its own.
    pub program: Option<String>,
    /// The host's Cargo, used to install the command.
    pub cargo: String,
    /// The local SSH client program: `$BASE_BENCHMARK_SSH`, or `ssh`.
    pub ssh: PathBuf,
    /// The local `rsync`: `$BASE_BENCHMARK_RSYNC`, or `rsync`.
    pub rsync: PathBuf,
    /// The local Cargo, used to find workspaces and path dependencies.
    pub local_cargo: PathBuf,
}

impl Remote {
    /// A host with the default directory, `.cache/base-benchmark`, and installed command.
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            directory: ".cache/base-benchmark".to_owned(),
            program: None,
            cargo: "cargo".to_owned(),
            ssh: program(std::env::var_os("BASE_BENCHMARK_SSH"), "ssh"),
            rsync: program(std::env::var_os("BASE_BENCHMARK_RSYNC"), "rsync"),
            local_cargo: crate::config::cargo_program(),
        }
    }

    /// Run `command` (`prepare`, `run`, or `smoke`) with its command-line `arguments` on the host. Progress goes to this process's standard error as it happens.
    ///
    /// Cancelling `cancellation` stops the host's command, as a termination signal would stop it locally, and its output is still copied back. The host's command also stops when this process exits or the connection drops.
    pub fn execute(
        &self,
        command: &str,
        arguments: Vec<String>,
        cancellation: &Cancellation,
    ) -> io::Result<Outcome> {
        if !matches!(command, "prepare" | "run" | "smoke") {
            return Err(invalid_input(format!(
                "{command} cannot run on another host; only prepare, run, and smoke can"
            )));
        }
        if self.host.is_empty() || self.host.starts_with('-') {
            return Err(invalid_input(format!(
                "{:?} is not an SSH destination",
                self.host
            )));
        }
        // Paths are checked here before the host is contacted; other arguments are checked by the host's command.
        let mut invocation = Invocation::parse(arguments)?;
        let manifest = existing(Path::new(
            invocation
                .take("manifest-path")
                .as_deref()
                .unwrap_or("Cargo.toml"),
        ))?;
        let workspace = self.workspace_root(&manifest)?;
        let mut uploads = vec![workspace.clone()];
        let mut local = vec![("manifest-path".to_owned(), Argument::Path(manifest))];
        let mut output = None;
        for (name, value) in std::mem::take(&mut invocation.named) {
            let argument = match name.as_str() {
                "output" => {
                    let path = located(Path::new(&value))?;
                    output = Some(path.clone());
                    Argument::Path(path)
                }
                "tree" => {
                    let (label, path) = value.split_once('=').ok_or_else(|| {
                        invalid_input(format!("expected LABEL=PATH, got {value:?}"))
                    })?;
                    let path = existing(Path::new(path))?;
                    uploads.push(path.clone());
                    Argument::Tree(label.to_owned(), path)
                }
                "artifacts" => Argument::Path(existing(Path::new(&value))?),
                _ => Argument::Plain(value),
            };
            local.push((name, argument));
        }
        // An explicit output, so that its location is known and can be downloaded.
        let output = output.unwrap_or_else(|| {
            let name = match command {
                "run" => format!(
                    "{}-{}",
                    invocation.positional.first().map_or("run", String::as_str),
                    local
                        .iter()
                        .find_map(|(name, argument)| match (name.as_str(), argument) {
                            ("mode", Argument::Plain(mode)) => Some(mode.as_str()),
                            _ => None,
                        })
                        .unwrap_or("screening")
                ),
                "prepare" => "prepared".to_owned(),
                _ => "smoke".to_owned(),
            };
            let output = crate::workspace::default_output(&workspace, &name);
            local.push(("output".to_owned(), Argument::Path(output.clone())));
            output
        });
        for tree in uploads.clone() {
            uploads.extend(self.path_dependencies(&tree));
        }
        uploads.sort();
        uploads.dedup();
        // A linked worktree's repository is elsewhere: mirror it too, unless it is already inside a mirrored directory.
        let linked = linked_repositories(&uploads);
        for repository in linked
            .iter()
            .flat_map(|linked| [&linked.common, &linked.git])
        {
            if !uploads.iter().any(|upload| repository.starts_with(upload)) {
                uploads.push(repository.clone());
            }
        }
        let job = Job {
            command,
            positional: invocation.positional,
            arguments: local,
            uploads,
            linked,
            output: &output,
        };
        self.resolved()?
            .run(job, cancellation)
            .map(|stdout| Outcome { stdout, output })
    }

    /// This remote with its directory made absolute against the remote user's home, since workloads start in the workspace root, where relative paths would not resolve.
    fn resolved(&self) -> io::Result<Self> {
        if self.directory.starts_with('/') {
            return Ok(self.clone());
        }
        let home = self.shell("reading the home directory", "printf '%s' \"$HOME\"", None)?;
        Ok(Self {
            directory: format!("{}/{}", home.trim_end_matches('/'), self.directory),
            ..self.clone()
        })
    }

    /// Upload, run the command with its paths mirrored, and download the output, also after a failure or cancellation.
    fn run(&self, job: Job, cancellation: &Cancellation) -> io::Result<String> {
        for path in &job.uploads {
            if cancellation.is_cancelled() {
                return Err(cancelled());
            }
            self.upload(path)?;
        }
        let program = self.program()?;
        let description = format!("base-benchmark {}", job.command);
        let mut words = vec![program, job.command.to_owned()];
        words.extend(job.positional);
        for (name, argument) in job.arguments {
            let value = match argument {
                Argument::Path(path) => self.mirror(&path),
                Argument::Tree(label, path) => format!("{label}={}", self.mirror(&path)),
                Argument::Plain(value) => value,
            };
            words.extend([format!("--{name}"), value]);
        }
        // Linked worktrees name their repository by its local path, so the mirrors name the mirrored repository instead.
        let mut setup: Vec<String> = job
            .linked
            .iter()
            .map(|linked| {
                format!(
                    "printf 'gitdir: %s\\n' {} > {}",
                    quote(&self.mirror(&linked.git)),
                    quote(&format!("{}/.git", self.mirror(&linked.directory)))
                )
            })
            .collect();
        // A mirrored output left by an earlier command, whose local copy is gone, would make the command fail for an output that is new here.
        if !job.output.exists() {
            setup.push(format!("rm -rf {}", quote(&self.mirror(job.output))));
        }
        let command = words
            .iter()
            .map(|word| quote(word))
            .collect::<Vec<_>>()
            .join(" ");
        let result = self.shell(
            &description,
            &stoppable(&setup, &command),
            Some(cancellation),
        );
        // Copy the output back even after a failure, which a failed or cancelled campaign records, unless the command failed before creating it: then rsync's complaints about the missing directory would follow, and bury, the host's own error.
        let downloaded = if result.is_err() && self.absent(job.output) {
            Ok(())
        } else {
            self.download(job.output)
        };
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        result.and_then(|stdout| downloaded.map(|()| stdout))
    }

    /// The host's path for a local absolute path.
    pub fn mirror(&self, path: &Path) -> String {
        format!("{}/mirror{}", self.directory, path.display())
    }

    /// The workspace containing `manifest`, from `cargo locate-project`.
    fn workspace_root(&self, manifest: &Path) -> io::Result<PathBuf> {
        let output = Command::new(&self.local_cargo)
            .args([
                "locate-project",
                "--workspace",
                "--message-format",
                "plain",
                "--manifest-path",
            ])
            .arg(manifest)
            .stderr(Stdio::inherit())
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{}: cannot find its workspace",
                manifest.display()
            )));
        }
        let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        Ok(root.parent().unwrap_or(&root).to_owned())
    }

    /// Path packages outside `tree`, which its build needs at the same relative location. When Cargo metadata cannot be read, none are found, and a warning says so.
    fn path_dependencies(&self, tree: &Path) -> Vec<PathBuf> {
        let packages = Command::new(&self.local_cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--manifest-path",
            ])
            .arg(tree.join("Cargo.toml"))
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
            .map(|metadata| {
                external_path_packages(&metadata, tree)
                    .into_values()
                    .collect()
            });
        packages.unwrap_or_else(|| {
            eprintln!(
                "base-benchmark: cannot read the Cargo metadata of {}, so path dependencies outside it are not mirrored",
                tree.display()
            );
            Vec::new()
        })
    }

    /// Mirror a local directory on the host, without its Cargo target directory, deleting files the local copy no longer has.
    fn upload(&self, path: &Path) -> io::Result<()> {
        self.rsync(&[
            "--delete".to_owned(),
            "--exclude=/target/".to_owned(),
            format!("{}/", path.display()),
            format!("{}:{}/", self.host, self.mirror(path)),
        ])
    }

    /// Whether the host certainly has no mirror of `output`. If the host cannot say, as when the connection has dropped, the output may exist.
    fn absent(&self, output: &Path) -> bool {
        self.shell(
            "checking for the output",
            &format!("[ -e {} ] || printf absent", quote(&self.mirror(output))),
            None,
        )
        .is_ok_and(|stdout| stdout == "absent")
    }

    fn download(&self, output: &Path) -> io::Result<()> {
        self.rsync(&[
            format!("{}:{}/", self.host, self.mirror(output)),
            format!("{}/", output.display()),
        ])
    }

    fn rsync(&self, arguments: &[String]) -> io::Result<()> {
        // Protected arguments reach the remote rsync unsplit, so paths may contain spaces. `--rsh` is parsed by rsync, so the program is quoted.
        let status = Command::new(&self.rsync)
            .args(["--archive", "--mkpath", "--protect-args", "--rsh"])
            .arg(quote(&self.ssh.display().to_string()))
            .args(arguments)
            .stdin(Stdio::null())
            .status()
            .map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", self.rsync.display()))
            })?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "rsync {} failed with {status}",
                arguments.join(" ")
            )));
        }
        Ok(())
    }

    /// The command to run on the host, unquoted, installing this version when it is missing.
    fn program(&self) -> io::Result<String> {
        if let Some(program) = &self.program {
            return Ok(program.clone());
        }
        let version = env!("CARGO_PKG_VERSION");
        let root = format!("{}/tool", self.directory);
        let program = format!("{root}/bin/base-benchmark");
        self.shell(
            &format!("installing base-benchmark {version}"),
            &format!(
                "{program} version 2>/dev/null | grep -qx {expected} || {cargo} install --quiet --locked --root {root} base-benchmark@{version} >&2",
                program = quote(&program),
                expected = quote(&format!("base-benchmark {version}")),
                cargo = quote(&self.cargo),
                root = quote(&root),
            ),
            None,
        )?;
        Ok(program)
    }

    /// Run a POSIX shell script on the host and return its standard output. The script goes to `sh -s` on standard input, so the remote user's login shell, such as fish, never parses it.
    ///
    /// With a cancellation, standard input stays open until the script ends or the cancellation is cancelled, and SSH starts with SIGINT and SIGQUIT ignored, which it keeps, so a terminal's interrupt reaches only this process, which cancels. SSH stays in the terminal's process group, so it can still prompt there, for example for a password. A [`stoppable`] script stops its command when its input closes.
    fn shell(
        &self,
        description: &str,
        script: &str,
        cancellation: Option<&Cancellation>,
    ) -> io::Result<String> {
        // Ignored signals stay ignored across `exec`, and SSH keeps them so.
        let mut ssh = match cancellation {
            Some(_) if cfg!(unix) => {
                let mut ssh = Command::new("/bin/sh");
                ssh.args(["-c", "trap '' INT QUIT; exec \"$0\" \"$@\""])
                    .arg(&self.ssh);
                ssh
            }
            _ => Command::new(&self.ssh),
        };
        ssh.args([self.host.as_str(), "sh", "-s"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = ssh.spawn().map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", self.ssh.display()))
        })?;
        let mut input = child.stdin.take().expect("stdin is piped");
        // A failure to write shows as the command's failure below.
        let _ = input.write_all(script.as_bytes());
        let finished = Arc::new(AtomicBool::new(false));
        let holder = cancellation.cloned().map(|cancellation| {
            let finished = finished.clone();
            std::thread::spawn(move || {
                while !finished.load(Ordering::SeqCst) && !cancellation.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(20));
                }
                drop(input);
            })
        });
        let result = child.wait_with_output();
        finished.store(true, Ordering::SeqCst);
        if let Some(holder) = holder {
            holder.join().expect("the input holder does not panic");
        }
        result.and_then(|output| {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            } else {
                Err(io::Error::other(format!(
                    "{description} on {} failed with {}",
                    self.host, output.status
                )))
            }
        })
    }
}

/// What to run remotely, with every local path it needs.
struct Job<'a> {
    command: &'a str,
    positional: Vec<String>,
    arguments: Vec<(String, Argument)>,
    uploads: Vec<PathBuf>,
    linked: Vec<Linked>,
    output: &'a Path,
}

fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "cancelled; the host's command was stopped",
    )
}

/// A script that runs `setup`, then `command`, stopping the command with SIGTERM when the script's standard input closes, as when the local side cancels, exits, or loses the connection.
///
/// The whole script is one compound command, so the shell has read all of it before anything else reads standard input.
fn stoppable(setup: &[String], command: &str) -> String {
    let setup = if setup.is_empty() {
        "true".to_owned()
    } else {
        setup.join(" && ")
    };
    format!(
        "{{\nexec 3<&0\n{setup} || exit $?\n{command} 3<&- &\npid=$!\n{{ while read -r _ <&3; do :; done; kill -TERM \"$pid\"; }} >/dev/null 2>&1 &\nwatcher=$!\nwait \"$pid\"\nstatus=$?\nkill \"$watcher\" 2>/dev/null\nexit \"$status\"\n}}\n"
    )
}

/// A directory whose `.git` is a file naming its Git directory elsewhere, as for a linked worktree or a submodule.
struct Linked {
    directory: PathBuf,
    /// Its absolute Git directory.
    git: PathBuf,
    /// The repository's common directory, which holds the objects, references, and usually the Git directory.
    common: PathBuf,
}

/// Every directory of `directories` whose `.git` is a file that Git can read.
fn linked_repositories(directories: &[PathBuf]) -> Vec<Linked> {
    directories
        .iter()
        .filter(|directory| directory.join(".git").is_file())
        .filter_map(|directory| {
            let output = Command::new("git")
                .arg("-C")
                .arg(directory)
                .args([
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-dir",
                    "--git-common-dir",
                ])
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|output| output.status.success());
            // One line for each directory asked for.
            output.and_then(|output| {
                let text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.trim_end()
                    .split_once('\n')
                    .map(|(git, common)| Linked {
                        directory: directory.clone(),
                        git: PathBuf::from(git),
                        common: PathBuf::from(common),
                    })
            })
        })
        .collect()
}

/// What a remote command produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The command's standard output, as it would print locally but with the host's paths.
    pub stdout: String,
    /// The local copy of its output directory: prepared artifacts, a campaign, or a smoke run's campaigns.
    pub output: PathBuf,
}

/// A command-line value: a local path to mirror, a labelled tree, or anything else, passed as it is.
enum Argument {
    Path(PathBuf),
    Tree(String, PathBuf),
    Plain(String),
}

/// `--name value` arguments, with positional ones kept in order.
struct Invocation {
    named: Vec<(String, String)>,
    positional: Vec<String>,
}

impl Invocation {
    fn parse(arguments: Vec<String>) -> io::Result<Self> {
        let mut invocation = Self {
            named: Vec::new(),
            positional: Vec::new(),
        };
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.strip_prefix("--") {
                Some(name) => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| invalid_input(format!("--{name} needs a value")))?;
                    invocation.named.push((name.to_owned(), value));
                }
                None => invocation.positional.push(argument),
            }
        }
        Ok(invocation)
    }

    fn take(&mut self, name: &str) -> Option<String> {
        let index = self.named.iter().position(|(key, _)| key == name)?;
        Some(self.named.remove(index).1)
    }
}

/// An existing local path, canonical, so that its mirror is the same however it was written. Errors name the path.
fn existing(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

/// A local output path as the mirror names it: its longest existing ancestor made canonical, as inputs are, so that a later command naming something inside it through the same symlink, such as `/tmp` on macOS, finds the same mirror; and the rest without `..` components.
fn located(path: &Path) -> io::Result<PathBuf> {
    let path = normalized(&std::path::absolute(path)?);
    // The root always exists, so an ancestor is always found; the normalized path is only a fallback.
    let found = path.ancestors().find_map(|ancestor| {
        let rest = path
            .strip_prefix(ancestor)
            .expect("an ancestor is a prefix");
        ancestor.canonicalize().ok().map(|canonical| {
            rest.components()
                .fold(canonical, |path, name| path.join(name))
        })
    });
    Ok(found.unwrap_or(path))
}

/// An absolute path without `..` components, which would let a mirrored path leave the mirror. Components of an absolute path never include `.`.
pub(crate) fn normalized(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            component => normal.push(component),
        }
    }
    normal
}

/// Quote a word for the POSIX shell that runs remote scripts.
pub(crate) fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests;
