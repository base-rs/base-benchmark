// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

/// Output shared with the test, which reads what the window wrote.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// Output that accepts `lines` lines and then fails, as a closed pipe does.
struct Closing {
    lines: usize,
}

impl Write for Closing {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.lines == 0 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        self.lines -= bytes.iter().filter(|byte| **byte == b'\n').count();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Input that cannot be read.
struct Unreadable;

impl io::Read for Unreadable {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::ConnectionReset))
    }
}

#[test]
fn markers_are_lines_and_follow_the_window_order() {
    let output = Shared::default();
    let mut window = Window::new(output.clone(), Cursor::new(Vec::new()));
    assert_eq!(
        window.stop().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(format!("{window:?}"), "Window { state: Before, .. }");
    window.start().unwrap();
    assert_eq!(
        window.start().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(format!("{window:?}"), "Window { state: Open, .. }");
    window.stop().unwrap();
    assert_eq!(
        window.stop().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(output.text(), format!("{WINDOW_START}\n{WINDOW_STOP}\n"));
}

#[test]
fn an_acknowledged_start_waits_for_the_runner() {
    let output = Shared::default();
    let mut window = Window::new(output.clone(), Cursor::new(format!("{WINDOW_STARTED}\r\n")));
    window.start_acknowledged().unwrap();
    assert_eq!(output.text(), format!("{WINDOW_START}\n"));
    for acknowledgement in ["", "something else\n"] {
        let mut window = Window::new(Shared::default(), Cursor::new(acknowledgement.to_owned()));
        let error = window.start_acknowledged().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
    }
    let mut started = Window::new(Shared::default(), Cursor::new(Vec::new()));
    started.start().unwrap();
    assert!(started.start_acknowledged().is_err());
    let mut closed = Window::new(Closing { lines: 0 }, Cursor::new(Vec::new()));
    assert_eq!(
        closed.start().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    let mut closing = Window::new(Closing { lines: 1 }, Cursor::new(Vec::new()));
    closing.start().unwrap();
    assert_eq!(
        closing.stop().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    let mut unread = Window::new(Shared::default(), io::BufReader::new(Unreadable));
    assert_eq!(
        unread.start_acknowledged().unwrap_err().kind(),
        io::ErrorKind::ConnectionReset
    );
}

#[test]
fn only_drivers_under_the_runner_have_a_window() {
    assert!(Window::from_control(None).unwrap().is_none());
    assert!(
        Window::from_control(Some(CONTROL_STDIO.into()))
            .unwrap()
            .is_some()
    );
    let error = Window::from_control(Some("tcp".into())).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(
        error
            .to_string()
            .contains("BASE_BENCHMARK_CONTROL is \"tcp\"")
    );
    assert!(Window::from_environment().unwrap().is_none());
}
