// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::ffi::OsString;
use std::io::{self, BufRead, Write};

/// The variable through which the runner tells a driver how to send its window markers. Its only value so far is [`CONTROL_STDIO`].
pub const CONTROL_VARIABLE: &str = "BASE_BENCHMARK_CONTROL";
/// Markers are whole lines on the driver's stdout, before its record, and the acknowledgement is a line on its stdin, which the runner then closes.
pub const CONTROL_STDIO: &str = "stdio";
/// The line a driver writes when its measured window starts.
pub const WINDOW_START: &str = "base-benchmark window start";
/// The line a driver writes when its measured window ends.
pub const WINDOW_STOP: &str = "base-benchmark window stop";
/// The line the runner writes back once it has taken its snapshots at the start of the window.
pub const WINDOW_STARTED: &str = "base-benchmark window started";

/// The measured window of a driver: the period of its counted operations, which it marks for the runner.
///
/// Under the runner, a multi-process scenario's driver marks when its counted operations start, after its connections are established and its warmup is done, and when they end, so that the runner can measure the services over exactly that window. Markers are messages, lines on stdout before the record, so they do not depend on the driver sharing a host with the runner. Each is a whole line, ending in a newline and flushed when it is sent, since the runner acts on markers as they arrive; a marker after output of the record, or one that ends the output without a newline, fails the sample. A driver that writes markers itself rather than through `Window` must do the same. The runner acknowledges the start once it has snapshotted every service, and then closes the driver's stdin, so the acknowledgement is the only line a driver reads there: [`Window::start`] does not wait for it, and [`Window::start_acknowledged`] does, so that the snapshot precedes the first counted operation. Stopping never waits.
///
/// A driver that sends no markers, sends one twice, or stops before it starts fails its sample. Outside the runner, [`Window::from_environment`] returns `None`, and the driver runs as it would without one.
///
/// ```no_run
/// use base_benchmark::Window;
///
/// let mut window = Window::from_environment()?;
/// // Connect and warm up, then:
/// if let Some(window) = &mut window {
///     window.start_acknowledged()?;
/// }
/// // The counted operations, then:
/// if let Some(window) = &mut window {
///     window.stop()?;
/// }
/// // Then write the record to stdout.
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct Window {
    output: Box<dyn Write + Send>,
    acknowledgements: Box<dyn BufRead + Send>,
    state: State,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Before,
    Open,
    Closed,
}

impl Window {
    /// The window of a driver the runner started, with markers on stdout and acknowledgements from stdin, or `None` outside the runner. A control method this version does not know is an error rather than ignored.
    pub fn from_environment() -> io::Result<Option<Self>> {
        Self::from_control(std::env::var_os(CONTROL_VARIABLE))
    }

    fn from_control(control: Option<OsString>) -> io::Result<Option<Self>> {
        match control {
            None => Ok(None),
            Some(control) if control == CONTROL_STDIO => Ok(Some(Self::new(
                io::stdout(),
                io::BufReader::new(io::stdin()),
            ))),
            Some(control) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "{CONTROL_VARIABLE} is {control:?}; this driver supports only {CONTROL_STDIO:?}"
                ),
            )),
        }
    }

    /// A window that writes markers to `output`, the stream the record follows on, and reads acknowledgements from `acknowledgements`.
    pub fn new(
        output: impl Write + Send + 'static,
        acknowledgements: impl BufRead + Send + 'static,
    ) -> Self {
        Self {
            output: Box::new(output),
            acknowledgements: Box::new(acknowledgements),
            state: State::Before,
        }
    }

    /// Mark the start of the window, without waiting for the runner's snapshots.
    pub fn start(&mut self) -> io::Result<()> {
        if self.state != State::Before {
            return Err(misuse("the window has already started"));
        }
        self.send(WINDOW_START)?;
        self.state = State::Open;
        Ok(())
    }

    /// Mark the start of the window and wait until the runner acknowledges it, so that its snapshots precede the first counted operation.
    pub fn start_acknowledged(&mut self) -> io::Result<()> {
        self.start()?;
        let mut line = String::new();
        self.acknowledgements.read_line(&mut line)?;
        if line.trim_end_matches(['\r', '\n']) != WINDOW_STARTED {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected the runner to acknowledge the window's start, but read {line:?}"),
            ));
        }
        Ok(())
    }

    /// Mark the end of the window. The runner snapshots the services without delaying the driver.
    pub fn stop(&mut self) -> io::Result<()> {
        if self.state != State::Open {
            return Err(misuse("the window is not open"));
        }
        self.send(WINDOW_STOP)?;
        self.state = State::Closed;
        Ok(())
    }

    fn send(&mut self, marker: &str) -> io::Result<()> {
        self.output.write_all(format!("{marker}\n").as_bytes())?;
        self.output.flush()
    }
}

impl std::fmt::Debug for Window {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Window")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

fn misuse(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
