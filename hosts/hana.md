# hana

Gathered 9 November 2026. hana, reached as `hana.local`, is a physical Linux server used as a measuring host. It is shared with light services, so it is quiet rather than isolated: a media server with occasional transcoding, SMB file sharing, a key-value store, a mail transfer agent, monitoring and endpoint security agents, ZFS's event daemon, SSH, and other users' sessions. Don't change its configuration, and don't use root.

## Hardware

|  |  |
| --- | --- |
| CPU | Intel Xeon E3-1240 v6 (Kaby Lake, family 6, model 158, stepping 9), microcode `0xf8`, one socket |
| Cores and threads | 4 cores, 8 threads; SMT siblings are CPUs `N` and `N + 4`: 0 and 4, 1 and 5, 2 and 6, 3 and 7 |
| Caches | 32 KiB L1 data and 32 KiB L1 instruction per core, 256 KiB L2 per core, 8 MiB L3 shared |
| Frequency | 3.7 GHz base, 4.1 GHz maximum turbo, 0.8 GHz minimum; busy CPUs ran at about 4.0 GHz with two to three cores loaded, in base-http's [client layout screen](https://github.com/base-rs/base-http/blob/main/benchmarks/hello/results/client-layout-20261109/readme.md) |
| Memory | 62 GiB (`MemTotal` 65,653,900 kB), 93 GiB swap; type and speed need root (`dmidecode`) and are not recorded |
| NUMA | One node, CPUs 0–7 |

## Software and settings

|  |  |
| --- | --- |
| Distribution and kernel | Arch Linux (rolling), Linux 7.2.14-arch1-1, `PREEMPT_DYNAMIC`, root on ZFS |
| Kernel command line | No CPU isolation, `nohz_full` or mitigation options: `isolated` and `nohz_full` are empty |
| Frequency scaling | `intel_pstate`, active, with hardware P-states; governor `powersave` and energy preference `balance_performance` on every CPU; turbo enabled (`no_turbo` 0), `hwp_dynamic_boost` 1 |
| SMT | On (`smt/control`) |
| Mitigations | Kernel defaults: PTI for Meltdown, IBRS for Spectre v2 and Retbleed, buffer clearing for MDS and MMIO stale data, which report "SMT vulnerable", TSX disabled; see `/sys/devices/system/cpu/vulnerabilities` |
| Network | Benchmarks use loopback: MTU 65,536, qdisc `noqueue`. Two four-queue Ethernet interfaces are bonded; their eight queue interrupts are spread one per CPU over CPUs 1–7 (CPU 4 has two), so every measuring CPU also takes the host's network interrupts. The SATA controller's interrupt is on CPU 0. Default IRQ affinity is all CPUs. |
| Network settings | `net.core.default_qdisc` `fq_codel`, `tcp_congestion_control` `cubic`, `somaxconn` 4096, `busy_poll` and `busy_read` 0 |
| Rust | 1.99.1, the distribution's package (`/usr/bin/cargo`) |

## CPU placement

Each benchmark records which CPUs its roles use on hana, and the measurements behind them, in its own repository, such as base-http's [hello benchmark](https://github.com/base-rs/base-http/blob/main/benchmarks/hello/readme.md#placement-on-hana). A placement relies on this topology:

- SMT siblings are CPUs `N` and `N + 4`, so CPUs 0, 1, 2 and 3 are on different cores, and roles that must not share a core also keep off each other's siblings among CPUs 4–7.
- There is one NUMA node and no isolated or `nohz_full` CPU, so every CPU also runs the host's services, and the network queue interrupts land on CPUs 1–7 and the SATA controller's on CPU 0.

## Measuring

- Take the host lock for every measurement, `flock /tmp/base-benchmark-host.lock`, as base-benchmark does, and check for another session's run, such as with `pgrep -af 'base-benchmark|wrk|hello-benchmark'`, waiting for it rather than interfering.
- Build and prepare before a campaign, not during one, and run nothing else while measuring.
- The host is quiet, not idle: single rounds of base-http's [client layout screen](https://github.com/base-rs/base-http/blob/main/benchmarks/hello/results/client-layout-20261109/readme.md) still ran up to 9% below their cohort's median, so interleave rounds and read ranges as well as medians.

## Gathered with

All without root, on 9 November 2026:

```sh
lscpu; lscpu -e; lscpu -C
grep -m1 'model name' /proc/cpuinfo; grep -m1 microcode /proc/cpuinfo
cat /sys/devices/system/cpu/cpu[0-3]/topology/thread_siblings_list
cat /sys/devices/system/cpu/cpu0/cpufreq/{scaling_driver,scaling_governor,energy_performance_preference,cpuinfo_min_freq,cpuinfo_max_freq,base_frequency}
cat /sys/devices/system/cpu/cpu*/cpufreq/{scaling_governor,energy_performance_preference} | sort | uniq -c
cat /sys/devices/system/cpu/intel_pstate/{status,no_turbo,hwp_dynamic_boost}
cat /sys/devices/system/cpu/{smt/control,isolated,nohz_full}
grep . /sys/devices/system/cpu/vulnerabilities/*
cat /proc/cmdline; uname -a; head -4 /etc/os-release
free -h; grep MemTotal /proc/meminfo; ls /sys/devices/system/node/
dmidecode -t memory    # needs root: not readable
ip -br link; ip link show lo
cat /proc/sys/net/core/{default_qdisc,somaxconn,busy_poll,busy_read} /proc/sys/net/ipv4/tcp_congestion_control
awk 'NR == 1 || /eno|enp|ahci/' /proc/interrupts; cat /proc/irq/default_smp_affinity
systemctl list-units --type=service --state=running
rustc --version; cargo --version
grep 'cpu MHz' /proc/cpuinfo    # sampled during measurement, as base-http's layout.py --frequencies does
```
