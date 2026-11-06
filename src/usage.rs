// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::record::{Metric, Phase, Sample, Source, Unit};
use std::io;
use std::time::Instant;

/// Cumulative CPU time and context switches for a set of threads or processes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceUsage {
    pub user_cpu_ns: u64,
    pub system_cpu_ns: u64,
    pub voluntary_context_switches: u64,
    pub involuntary_context_switches: u64,
}

impl ResourceUsage {
    /// The checked difference from an earlier snapshot. A decreasing counter is an error, not a zero.
    pub fn since(self, earlier: Self) -> io::Result<Self> {
        let delta = |later: u64, earlier: u64, name: &str| {
            later.checked_sub(earlier).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} decreased between snapshots"),
                )
            })
        };
        Ok(Self {
            user_cpu_ns: delta(self.user_cpu_ns, earlier.user_cpu_ns, "user CPU time")?,
            system_cpu_ns: delta(self.system_cpu_ns, earlier.system_cpu_ns, "system CPU time")?,
            voluntary_context_switches: delta(
                self.voluntary_context_switches,
                earlier.voluntary_context_switches,
                "voluntary context switches",
            )?,
            involuntary_context_switches: delta(
                self.involuntary_context_switches,
                earlier.involuntary_context_switches,
                "involuntary context switches",
            )?,
        })
    }
}

/// A source of resource snapshots. Tests and alternative scopes, such as `/proc` readers, can supply their own.
pub trait ResourceSource {
    fn snapshot(&mut self) -> io::Result<ResourceUsage>;

    /// Resolution of CPU times in nanoseconds.
    fn resolution_ns(&self) -> u64;
}

/// `getrusage(RUSAGE_SELF)`: every thread of the current process, at microsecond resolution.
#[derive(Clone, Copy, Debug, Default)]
pub struct CurrentProcess;

impl ResourceSource for CurrentProcess {
    #[cfg(unix)]
    fn snapshot(&mut self) -> io::Result<ResourceUsage> {
        use nix::sys::resource::{UsageWho, getrusage};
        use nix::sys::time::TimeValLike;
        // `getrusage` fails only for an invalid `who` or address, neither of which is possible here.
        let usage = getrusage(UsageWho::RUSAGE_SELF).expect("getrusage(RUSAGE_SELF) succeeds");
        Ok(ResourceUsage {
            user_cpu_ns: usage.user_time().num_microseconds().unsigned_abs() * 1000,
            system_cpu_ns: usage.system_time().num_microseconds().unsigned_abs() * 1000,
            voluntary_context_switches: usage.voluntary_context_switches().unsigned_abs(),
            involuntary_context_switches: usage.involuntary_context_switches().unsigned_abs(),
        })
    }

    #[cfg(not(unix))]
    fn snapshot(&mut self) -> io::Result<ResourceUsage> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process resource usage requires Unix getrusage",
        ))
    }

    fn resolution_ns(&self) -> u64 {
        1000
    }
}

/// Measures one batch of operations: wall time, CPU time, and context switches.
///
/// The resource snapshots surround the wall-clock interval, so the CPU window is slightly wider than the wall window. Both are reported in the measured phase because they bracket the same operations and nothing else.
pub struct Interval<S: ResourceSource = CurrentProcess> {
    source: S,
    usage: ResourceUsage,
    start: Instant,
}

impl Interval<CurrentProcess> {
    /// Start measuring the current process.
    pub fn start() -> io::Result<Self> {
        Self::start_with(CurrentProcess)
    }
}

impl<S: ResourceSource> Interval<S> {
    pub fn start_with(mut source: S) -> io::Result<Self> {
        let usage = source.snapshot()?;
        Ok(Self {
            source,
            usage,
            start: Instant::now(),
        })
    }

    /// Finish measuring `operations` and return a sample containing the standard metrics.
    pub fn finish(mut self, operations: u64) -> io::Result<Sample> {
        let wall_ns = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let usage = self.source.snapshot()?.since(self.usage)?;
        if operations == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an interval must measure at least one operation",
            ));
        }
        let resolution = self.source.resolution_ns();
        let cpu = |name: &str, value| {
            Metric::total(name, Unit::Nanoseconds, value)
                .with_source(Source::Rusage)
                .with_resolution(resolution)
                .lower_is_better()
        };
        let switches = |name: &str, value| {
            Metric::total(name, Unit::Count, value)
                .with_source(Source::Rusage)
                .lower_is_better()
        };
        Ok(Sample::new(operations)
            .with(
                Metric::total("wall_time", Unit::Nanoseconds, wall_ns)
                    .with_source(Source::Wall)
                    .with_phase(Phase::Measured)
                    .lower_is_better(),
            )
            .with(cpu("user_cpu_time", usage.user_cpu_ns))
            .with(cpu("system_cpu_time", usage.system_cpu_ns))
            .with(switches(
                "voluntary_context_switches",
                usage.voluntary_context_switches,
            ))
            .with(switches(
                "involuntary_context_switches",
                usage.involuntary_context_switches,
            )))
    }
}

#[cfg(test)]
mod tests;
