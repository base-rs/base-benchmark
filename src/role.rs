// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Multi-process roles: a scenario's services and driver, declared in `[[package.metadata.base-benchmark.scenario.role]]`.
//!
//! Each role is one supervised process, in its own process group, for every sample. Services start in declared order, each waited on until it prints a line matching its ready pattern; values the pattern captures, such as the address the server bound, are passed to later roles' arguments as `{ROLE.NAME}`. Then the driver runs to completion, and the services are stopped in reverse order. Nothing here assumes that roles share a host: endpoints reach later roles only as captured values.

use crate::config::{invalid_input, valid_label};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;

/// Whether a role is a service, which is started, waited on until ready, and stopped, or the driver, which runs to completion and owns the measured window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleKind {
    Service,
    Driver,
}

/// What a role runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum RoleProgram {
    /// The artifact and variant being compared, with the scenario's arguments, as a single-process scenario runs it.
    Subject,
    /// A program outside the workspace, such as nginx, whose path on the measuring host is given when the scenario runs, never in package metadata.
    External { name: String },
}

/// A line a service prints on stdout once it is ready, such as `{address}` for a server that prints only the address it bound.
///
/// The pattern matches a whole line. Text outside braces must appear exactly; each `{NAME}` captures a non-empty part of the line, which later roles' arguments receive as `{ROLE.NAME}`. A capture takes the longest text that lets the rest of the line match, so `{host}:{port}` splits `[::1]:8080` at its last colon. `{{` and `}}` stand for literal braces. Two captures must be separated by text. Lines before the first match are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyLine {
    pub pattern: String,
    /// How long the service may take to print it, from its start.
    pub timeout_ms: u64,
}

/// How a service is stopped once the driver has finished.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Stop {
    /// Send `signal`, such as `SIGTERM`, to its process group, and kill the group if it has not exited after `grace_ms`. It stops successfully when it exits with status 0 or dies from `signal`.
    Signal { signal: String, grace_ms: u64 },
    /// Wait for it to exit by itself, with status 0, within the session's deadline. It fails if it exits before the driver has finished.
    Exit,
}

/// The signals a service may be stopped with, by name. Numbers differ between systems, so plans name them.
pub const STOP_SIGNALS: [&str; 6] = [
    "SIGTERM", "SIGINT", "SIGHUP", "SIGQUIT", "SIGUSR1", "SIGUSR2",
];

/// The default time a service has to print its ready line.
pub const DEFAULT_READY_TIMEOUT_MS: u64 = 10_000;
/// The default time a service has to exit after its stop signal before its group is killed.
pub const DEFAULT_STOP_GRACE_MS: u64 = 10_000;

/// A declared role. The subject's arguments are the scenario's own, so a role that runs the subject declares none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub name: String,
    pub kind: RoleKind,
    pub program: RoleProgram,
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Appended to `arguments` only in smoke runs.
    #[serde(default)]
    pub smoke_arguments: Vec<String>,
    /// Appended to `arguments` only in screening and recorded runs.
    #[serde(default)]
    pub measure_arguments: Vec<String>,
    /// Services only.
    #[serde(default)]
    pub ready: Option<ReadyLine>,
    /// Services only.
    #[serde(default)]
    pub stop: Option<Stop>,
}

/// `[[package.metadata.base-benchmark.scenario.role]]`.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct RoleConfiguration {
    name: String,
    kind: RoleKind,
    #[serde(default)]
    subject: bool,
    external: Option<String>,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    smoke_arguments: Vec<String>,
    #[serde(default)]
    measure_arguments: Vec<String>,
    ready: Option<String>,
    ready_timeout_seconds: Option<f64>,
    stop: Option<String>,
    stop_grace_seconds: Option<f64>,
}

impl RoleConfiguration {
    /// Resolve defaults; `context` prefixes errors with the package and scenario.
    pub(crate) fn resolve(self, context: &dyn Fn(String) -> io::Error) -> io::Result<Role> {
        let role = |message: String| context(format!("role {:?}: {message}", self.name));
        let program = match (self.subject, &self.external) {
            (true, None) => RoleProgram::Subject,
            (false, Some(name)) if valid_label(name) => {
                RoleProgram::External { name: name.clone() }
            }
            (false, Some(name)) => {
                return Err(role(format!(
                    "external program names may contain only ASCII letters, digits, '.', '_', and '-', not {name:?}"
                )));
            }
            _ => {
                return Err(role(
                    "declare exactly one program: `subject = true` or `external = \"NAME\"`"
                        .to_owned(),
                ));
            }
        };
        let milliseconds = |name: &str, seconds: Option<f64>, default: u64| match seconds {
            None => Ok(default),
            Some(seconds) if seconds.is_finite() && seconds > 0.0 && seconds <= 86_400.0 => {
                Ok(((seconds * 1000.0).ceil() as u64).max(1))
            }
            Some(seconds) => Err(role(format!(
                "{name} must be positive and at most a day, not {seconds}"
            ))),
        };
        let (ready, stop) = match self.kind {
            RoleKind::Service => {
                let pattern = self.ready.ok_or_else(|| {
                    role("a service needs `ready`, the line it prints once it is ready".to_owned())
                })?;
                Pattern::parse(&pattern).map_err(|error| role(error.to_string()))?;
                let ready = ReadyLine {
                    pattern,
                    timeout_ms: milliseconds(
                        "ready-timeout-seconds",
                        self.ready_timeout_seconds,
                        DEFAULT_READY_TIMEOUT_MS,
                    )?,
                };
                let stop = match self.stop.as_deref() {
                    Some("exit") if self.stop_grace_seconds.is_some() => {
                        return Err(role(
                            "a service that stops by exiting has no stop-grace-seconds".to_owned(),
                        ));
                    }
                    Some("exit") => Stop::Exit,
                    signal => {
                        let signal = signal.unwrap_or("SIGTERM");
                        if !STOP_SIGNALS.contains(&signal) {
                            return Err(role(format!(
                                "stop must be \"exit\" or one of {}, not {signal:?}",
                                STOP_SIGNALS.join(", ")
                            )));
                        }
                        Stop::Signal {
                            signal: signal.to_owned(),
                            grace_ms: milliseconds(
                                "stop-grace-seconds",
                                self.stop_grace_seconds,
                                DEFAULT_STOP_GRACE_MS,
                            )?,
                        }
                    }
                };
                (Some(ready), Some(stop))
            }
            RoleKind::Driver => {
                if self.ready.is_some()
                    || self.ready_timeout_seconds.is_some()
                    || self.stop.is_some()
                    || self.stop_grace_seconds.is_some()
                {
                    return Err(role(
                        "a driver runs to completion, so it has no ready line or stop".to_owned(),
                    ));
                }
                (None, None)
            }
        };
        if program == RoleProgram::Subject
            && !(self.arguments.is_empty()
                && self.smoke_arguments.is_empty()
                && self.measure_arguments.is_empty())
        {
            return Err(role(
                "the subject's arguments are the scenario's own `arguments`, `smoke-arguments`, `measure-arguments`, and variants".to_owned(),
            ));
        }
        Ok(Role {
            name: self.name,
            kind: self.kind,
            program,
            arguments: self.arguments,
            smoke_arguments: self.smoke_arguments,
            measure_arguments: self.measure_arguments,
            ready,
            stop,
        })
    }
}

/// Check a scenario's roles together: names, one driver, one subject, and that every `{ROLE.NAME}` reference names a value captured by a service that starts before the role using it. `subject_arguments` are every argument the subject may receive.
pub(crate) fn validate_roles(
    roles: &[Role],
    subject_arguments: &[&String],
    context: &dyn Fn(String) -> io::Error,
) -> io::Result<()> {
    if roles.is_empty() {
        return Ok(());
    }
    let mut names = BTreeSet::new();
    for role in roles {
        // A role name and a capture name are joined by `.` in references.
        if !valid_label(&role.name) || role.name.contains('.') || !names.insert(role.name.as_str())
        {
            return Err(context(format!(
                "invalid or duplicate role name {:?}; names may contain only ASCII letters, digits, '_', and '-'",
                role.name
            )));
        }
    }
    let count = |kind: RoleKind| roles.iter().filter(|role| role.kind == kind).count();
    if count(RoleKind::Driver) != 1 {
        return Err(context(format!(
            "a scenario with roles has exactly one driver, not {}",
            count(RoleKind::Driver)
        )));
    }
    let subjects = roles
        .iter()
        .filter(|role| role.program == RoleProgram::Subject)
        .count();
    if subjects != 1 {
        return Err(context(format!(
            "exactly one role runs the subject, not {subjects}"
        )));
    }
    // Services start in declared order, and the driver after all of them.
    let mut available: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let order = roles
        .iter()
        .filter(|role| role.kind == RoleKind::Service)
        .chain(roles.iter().filter(|role| role.kind == RoleKind::Driver));
    for role in order {
        let arguments: Vec<&String> = if role.program == RoleProgram::Subject {
            subject_arguments.to_vec()
        } else {
            role.arguments
                .iter()
                .chain(&role.smoke_arguments)
                .chain(&role.measure_arguments)
                .collect()
        };
        for argument in arguments {
            for (target, value) in references(argument, &names) {
                let known = available
                    .get(target)
                    .is_some_and(|captures| captures.contains(value));
                if !known {
                    return Err(context(format!(
                        "role {:?}: {{{target}.{value}}} names no value captured by a service that is ready before it starts",
                        role.name
                    )));
                }
            }
        }
        if let Some(ready) = &role.ready {
            let pattern =
                Pattern::parse(&ready.pattern).expect("resolved roles have valid ready patterns");
            available.insert(&role.name, pattern.captures().map(str::to_owned).collect());
        }
    }
    Ok(())
}

/// Every `{ROLE.NAME}` in `argument` whose ROLE is one of `roles`. Other braces are left alone, so arguments can contain JSON.
fn references<'a>(argument: &'a str, roles: &BTreeSet<&str>) -> Vec<(&'a str, &'a str)> {
    let mut found = Vec::new();
    let mut rest = argument;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find(['{', '}']) else {
            break;
        };
        if after.as_bytes()[end] == b'}' {
            if let Some((role, name)) = after[..end].split_once('.') {
                if roles.contains(role) {
                    found.push((role, name));
                }
            }
        }
        rest = &after[end..];
    }
    found
}

/// Replace every `{ROLE.NAME}` whose value `captured` holds.
pub(crate) fn substitute(
    argument: &str,
    captured: &BTreeMap<String, BTreeMap<String, String>>,
) -> String {
    let mut output = String::with_capacity(argument.len());
    let mut rest = argument;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let value = after.find(['{', '}']).and_then(|end| {
            let (role, name) = after[..end].split_once('.')?;
            (after.as_bytes()[end] == b'}')
                .then(|| captured.get(role)?.get(name))
                .flatten()
                .map(|value| (value, end))
        });
        match value {
            Some((value, end)) => {
                output.push_str(value);
                rest = &after[end + 1..];
            }
            None => {
                output.push('{');
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

/// A parsed ready pattern: literal text, then each capture with the literal text that follows it, empty only after the last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern {
    prefix: String,
    captures: Vec<(String, String)>,
}

impl Pattern {
    pub(crate) fn parse(pattern: &str) -> io::Result<Self> {
        let invalid =
            |message: &str| invalid_input(format!("ready pattern {pattern:?}: {message}"));
        let mut parsed = Self {
            prefix: String::new(),
            captures: Vec::new(),
        };
        let mut literal = String::new();
        let mut characters = pattern.chars().peekable();
        while let Some(character) = characters.next() {
            match (character, characters.peek()) {
                ('{', Some('{')) | ('}', Some('}')) => {
                    literal.push(character);
                    characters.next();
                }
                ('{', _) => {
                    let mut name = String::new();
                    loop {
                        match characters.next() {
                            Some('}') => break,
                            Some(character) => name.push(character),
                            None => return Err(invalid("a `{` is not closed")),
                        }
                    }
                    let valid = !name.is_empty()
                        && name
                            .chars()
                            .all(|character| character.is_ascii_alphanumeric() || character == '_');
                    if !valid {
                        return Err(invalid(&format!(
                            "capture names may contain only ASCII letters, digits, and '_', not {name:?}"
                        )));
                    }
                    if parsed.captures.iter().any(|(other, _)| *other == name) {
                        return Err(invalid(&format!("{{{name}}} is captured twice")));
                    }
                    let literal = std::mem::take(&mut literal);
                    match parsed.captures.last_mut() {
                        None => parsed.prefix = literal,
                        Some(_) if literal.is_empty() => {
                            return Err(invalid("captures must be separated by text"));
                        }
                        Some((_, following)) => *following = literal,
                    }
                    parsed.captures.push((name, String::new()));
                }
                ('}', _) => return Err(invalid("a `}` is not opened; write `}}` for a brace")),
                (character, _) => literal.push(character),
            }
        }
        match parsed.captures.last_mut() {
            None if literal.is_empty() => return Err(invalid("the pattern is empty")),
            None => parsed.prefix = literal,
            Some((_, following)) => *following = literal,
        }
        Ok(parsed)
    }

    pub(crate) fn captures(&self) -> impl Iterator<Item = &str> {
        self.captures.iter().map(|(name, _)| name.as_str())
    }

    /// The values captured from `line`, if the whole line matches.
    pub(crate) fn matches(&self, line: &str) -> Option<BTreeMap<String, String>> {
        let mut captures = BTreeMap::new();
        line.strip_prefix(self.prefix.as_str())
            .is_some_and(|rest| match_captures(&self.captures, rest, &mut captures))
            .then_some(captures)
    }
}

/// Match captures, each followed by its literal text, against the whole of `text`, trying the longest value of each capture first.
fn match_captures(
    captures: &[(String, String)],
    text: &str,
    values: &mut BTreeMap<String, String>,
) -> bool {
    match captures {
        [] => text.is_empty(),
        [(name, following), rest @ ..] => (1..=text.len())
            .rev()
            .filter(|end| {
                text.is_char_boundary(*end) && text[*end..].starts_with(following.as_str())
            })
            .any(|end| {
                values.insert(name.clone(), text[..end].to_owned());
                match_captures(rest, &text[end + following.len()..], values)
            }),
    }
}

#[cfg(test)]
mod tests;
