// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

fn main() -> std::process::ExitCode {
    base_benchmark::cli::main(
        std::env::args().skip(1).collect(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}
