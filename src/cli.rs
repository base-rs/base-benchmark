// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The `base-benchmark` command. It parses arguments and calls [`Workspace`] operations, exactly as the Bake tasks do.
use crate::config::invalid_input;
use crate::plan::parse_cpus;
use crate::prepare::Tree;
use crate::remote::Remote;
use crate::runner::{Cancellation, summarize_directory};
use crate::workspace::{PrepareRequest, RunRequest, Workspace};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;

const USAGE: &str = "\
usage: base-benchmark COMMAND [--name value]...

  list       [--manifest-path PATH]
  prepare    [--manifest-path PATH] [--package NAME] [--tree LABEL=PATH]... [--replicas N] [--output DIR]
             [--scenario NAME]... [--overlay true]
  smoke      [--manifest-path PATH] [--package NAME] [--output DIR] [--lock PATH] [--program NAME=PATH]...
  run        SCENARIO --artifacts FILE [--mode smoke|screening|recorded] [--package NAME]
             [--manifest-path PATH] [--rounds N] [--seed N] [--cpus 0,2] [--role-cpus ROLE=0,2]...
             [--program NAME=PATH]... [--perf-event EVENT]... [--timeout-seconds N] [--lock PATH]
             [--output DIR] [--decision-rule TEXT] [--idle-host true] [--stack-padding false]
  summarize  DIR
  version

--cpus places a single-process scenario's workloads; --role-cpus places each role of a multi-process scenario.
--program gives the path of an external program a role runs, on the host that runs the roles.

Prepare, run, and smoke also accept --host HOST [--remote-directory DIR] [--remote-program PATH] to run on
another host over SSH, mirroring local paths there with rsync and copying the output back. Program paths name
files on that host and pass unchanged.

Run, smoke, and summarize print a report on stdout; list and prepare print JSON. Progress goes to stderr.";

/// Run the command with `arguments` (excluding the program name).
pub fn main(arguments: Vec<String>, out: &mut dyn Write, errors: &mut dyn Write) -> ExitCode {
    match execute(arguments.clone(), out, errors) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(errors, "base-benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(arguments: Vec<String>, out: &mut dyn Write, errors: &mut dyn Write) -> io::Result<()> {
    let invocation = std::iter::once("base-benchmark".to_owned())
        .chain(arguments.iter().cloned())
        .collect();
    let mut arguments = arguments.into_iter();
    let command = arguments.next().unwrap_or_default();
    let mut options = Options::parse(arguments)?;
    if matches!(command.as_str(), "prepare" | "run" | "smoke") {
        if let Some(host) = options.take("host")? {
            let mut remote = Remote::new(host);
            if let Some(directory) = options.take("remote-directory")? {
                remote.directory = directory;
            }
            remote.program = options.take("remote-program")?;
            let arguments = std::mem::take(&mut options.positional)
                .into_iter()
                .chain(
                    std::mem::take(&mut options.named)
                        .into_iter()
                        .flat_map(|(name, value)| [format!("--{name}"), value]),
                )
                .collect();
            // A termination signal cancels the host's command, whose output is still copied back.
            let cancellation = Cancellation::new();
            let _signals = cancellation.on_termination_signals();
            let outcome = remote.execute(&command, arguments, &cancellation)?;
            return write!(out, "{}", outcome.stdout);
        }
    }
    let cancellation = Cancellation::new();
    match command.as_str() {
        "list" => {
            let workspace = Workspace::new(options.path("manifest-path")?);
            options.finish(0)?;
            print_json(out, &workspace.suites()?)
        }
        "prepare" => {
            let workspace = Workspace::new(options.path("manifest-path")?);
            let package = options.take("package")?;
            let trees = options
                .take_all("tree")
                .iter()
                .map(|tree| tree.parse())
                .collect::<io::Result<Vec<Tree>>>()?;
            let request = PrepareRequest {
                package,
                trees,
                replicas: options.parsed("replicas")?,
                output: options.take("output")?.map(PathBuf::from),
                scenarios: options.take_all("scenario"),
                overlay: options.parsed("overlay")?.unwrap_or(false),
            };
            options.finish(0)?;
            let _signals = cancellation.on_termination_signals();
            print_json(out, &workspace.prepare(request, &cancellation)?)
        }
        "smoke" => {
            let workspace = Workspace::new(options.path("manifest-path")?);
            let workspace = options.programs(workspace)?;
            let package = options.take("package")?;
            let output = options.take("output")?.map(PathBuf::from);
            let lock = options.take("lock")?.map(PathBuf::from);
            options.finish(0)?;
            let _signals = cancellation.on_termination_signals();
            for campaign in
                workspace.smoke(package.as_deref(), output, lock, &cancellation, errors)?
            {
                writeln!(out, "{}\n{}", campaign.output.display(), campaign.report)?;
            }
            Ok(())
        }
        "run" => {
            let workspace = Workspace::new(options.path("manifest-path")?);
            let workspace = options.programs(workspace)?;
            let request = RunRequest {
                package: options.take("package")?,
                mode: options.parsed("mode")?,
                rounds: options.parsed("rounds")?,
                seed: options.parsed("seed")?,
                cpus: options
                    .take("cpus")?
                    .map(|cpus| parse_cpus(&cpus))
                    .transpose()?
                    .unwrap_or_default(),
                role_cpus: options
                    .take_all("role-cpus")
                    .iter()
                    .map(|value| {
                        let (role, cpus) = assignment(value, "ROLE=CPUS")?;
                        Ok((role.to_owned(), parse_cpus(cpus)?))
                    })
                    .collect::<io::Result<_>>()?,
                perf_events: options.take_all("perf-event"),
                timeout: options.parsed("timeout-seconds")?.map(Duration::from_secs),
                lock: options.take("lock")?.map(PathBuf::from),
                output: options.take("output")?.map(PathBuf::from),
                decision_rule: options.take("decision-rule")?,
                idle_host: options.parsed("idle-host")?.unwrap_or(false),
                stack_padding: options.parsed("stack-padding")?,
                invocation,
                ..RunRequest::new(String::new(), options.required("artifacts")?)
            };
            options.finish(1)?;
            let request = RunRequest {
                scenario: options.positional.remove(0),
                ..request
            };
            let _signals = cancellation.on_termination_signals();
            let campaign = workspace.run(request, &cancellation, errors)?;
            writeln!(out, "{}\n{}", campaign.output.display(), campaign.report)
        }
        "summarize" => {
            options.finish(1)?;
            let campaign = summarize_directory(&PathBuf::from(&options.positional[0]))?;
            write!(out, "{}", campaign.report)
        }
        "version" | "--version" => writeln!(out, "base-benchmark {}", env!("CARGO_PKG_VERSION")),
        "help" | "--help" | "-h" => writeln!(out, "{USAGE}"),
        _ => Err(invalid_input(format!(
            "unknown command {command:?}\n{USAGE}"
        ))),
    }
}

/// Split `NAME=VALUE`, where neither may be empty.
fn assignment<'a>(value: &'a str, form: &str) -> io::Result<(&'a str, &'a str)> {
    value
        .split_once('=')
        .filter(|(name, value)| !name.is_empty() && !value.is_empty())
        .ok_or_else(|| invalid_input(format!("expected {form}, got {value:?}")))
}

fn print_json(out: &mut dyn Write, value: &impl serde::Serialize) -> io::Result<()> {
    let mut text = serde_json::to_vec_pretty(value).expect("command results serialize");
    text.push(b'\n');
    out.write_all(&text)
}

/// `--name value` pairs and positional arguments. Unknown and repeated single-valued options are errors.
struct Options {
    named: Vec<(String, String)>,
    positional: Vec<String>,
}

impl Options {
    fn empty() -> Self {
        Self {
            named: Vec::new(),
            positional: Vec::new(),
        }
    }

    fn parse(arguments: impl Iterator<Item = String>) -> io::Result<Self> {
        let mut options = Self::empty();
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            match argument.strip_prefix("--") {
                Some(name) => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| invalid_input(format!("--{name} needs a value")))?;
                    options.named.push((name.to_owned(), value));
                }
                None => options.positional.push(argument),
            }
        }
        Ok(options)
    }

    fn take_all(&mut self, name: &str) -> Vec<String> {
        let (taken, remaining) = std::mem::take(&mut self.named)
            .into_iter()
            .partition(|(key, _)| key == name);
        self.named = remaining;
        taken.into_iter().map(|(_, value)| value).collect()
    }

    fn take(&mut self, name: &str) -> io::Result<Option<String>> {
        let mut values = self.take_all(name);
        if values.len() > 1 {
            return Err(invalid_input(format!("--{name} was given more than once")));
        }
        Ok(values.pop())
    }

    fn required(&mut self, name: &str) -> io::Result<String> {
        self.take(name)?
            .ok_or_else(|| invalid_input(format!("--{name} is required")))
    }

    /// Add every `--program NAME=PATH` to `workspace`.
    fn programs(&mut self, mut workspace: Workspace) -> io::Result<Workspace> {
        for value in self.take_all("program") {
            let (name, path) = assignment(&value, "NAME=PATH")?;
            if workspace.programs.contains_key(name) {
                return Err(invalid_input(format!(
                    "--program {name} was given more than once"
                )));
            }
            workspace = workspace.with_program(name, path);
        }
        Ok(workspace)
    }

    /// The workspace manifest, `Cargo.toml` by default.
    fn path(&mut self, name: &str) -> io::Result<PathBuf> {
        Ok(PathBuf::from(
            self.take(name)?.unwrap_or_else(|| "Cargo.toml".to_owned()),
        ))
    }

    fn parsed<T: FromStr>(&mut self, name: &str) -> io::Result<Option<T>> {
        self.take(name)?
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| invalid_input(format!("invalid --{name} value {value:?}")))
            })
            .transpose()
    }

    fn finish(&self, positional: usize) -> io::Result<()> {
        if let Some((name, _)) = self.named.first() {
            return Err(invalid_input(format!("unknown option --{name}")));
        }
        if self.positional.len() != positional {
            return Err(invalid_input(format!(
                "expected {positional} positional argument(s), got {:?}",
                self.positional
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
