# Benchmark hosts

Each host that Base projects measure their benchmarks on has one document here, named after the host, such as [hana.md](hana.md). Every repository that measures on a host shares its document: link to it, from another repository by its absolute URL, such as `https://github.com/base-rs/base-benchmark/blob/main/hosts/hana.md`, rather than repeat the host's hardware or settings.

A host document records:

- what the host is and how to reach it, and what else runs on it, by kind of service, without naming people;
- its hardware: CPU model, cores, threads and the SMT sibling layout, caches, base and turbo frequency, memory size and, where readable, type and speed, and NUMA nodes;
- its software and settings: frequency scaling driver, governor and energy preference, kernel, distribution and command line, CPU vulnerability mitigations, network interfaces and their interrupt placement, and loopback settings, since benchmarks run over loopback, and the Rust toolchain used;
- how to measure there without disturbing other sessions or being disturbed by them;
- the date it was gathered and the commands that gathered it, all readable without root, so that it can be gathered again.

A benchmark's CPU placement on a host, which CPUs its client, server, controller and runner use and which SMT siblings stay idle, depends on that benchmark's roles and on measurements of it, so it stays with the benchmark, in its own repository, and links to the host document for the topology it relies on. A placement changes only with measurements behind it.

Gather a host document again when the host's kernel, firmware, settings or toolchain change, and keep the previous values where results depend on them.
