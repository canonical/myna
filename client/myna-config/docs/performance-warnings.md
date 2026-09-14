# Performance warnings

Dictation runs inference on the CPU and is only usable at full clock. The
diagnostics page therefore measures, on every refresh, whether this machine is
currently delivering that, and says so in a warning with a cause and a remedy.
A warning never blocks the ready state: the machine is set up, it is just not
delivering.

## Why a measurement and not a reading

An idle core legitimately rests at `scaling_min_freq`, so a snapshot of
`scaling_cur_freq` cannot distinguish a healthy laptop from a throttled one.
`myna_config::performance::spin_probe` loads one core per frequency class from
a pinned thread for 300 ms and records the highest `scaling_cur_freq` it
reached. One core, not all of them: the question is whether a core is allowed
to boost, and an all-core load on a thin laptop legitimately settles well
below the single-core maximum.

A frequency class is every core that shares a `cpuinfo_max_freq`.
Heterogeneous parts expose several (a Zen 5 laptop has a 5.1 GHz class and a
3.5 GHz class), and each is judged against its own ceiling. The worst class
decides.

## Thresholds

`CLAMP_FRACTION_PERCENT` is 50. A healthy single-core boost reaches its
maximum or close to it; a firmware clamp leaves the core at 10-20 % of it.
Half is far from both, which is what keeps a slightly warm laptop from
raising a false alarm.

The cause is attributed in the order a user should try fixes:

1. `scaling_max_freq` below half the hardware maximum: a software cap. The
   user, a power tool, or the boot line set it and can undo it.
2. `platform_profile` is `low-power`: a settings change.
3. Otherwise: a clamp below the kernel. The governor, the energy-performance
   preference, and the platform profile were all verified unable to lift it
   on the machine this was written against. The remedy is the firmware-level
   action: unplug and replug the charger, or power off fully.

Pressure-stall information (`/proc/pressure`, ten-second `avg10`) is read at
the same time. Memory `some` and I/O `full` at or above 10 % warn: the
machine is swapping or thrashing. CPU `some` warns only at 80 % or above,
because a build in another window is not a fault.

No cpufreq tree, no PSI, or a probe that returns nothing all render as
"unknown" and omit the line. A confined or unusual host must degrade to
silence, never to a false alarm.

## The case this was written against

Framework Laptop 13 (AMD Ryzen AI 300), BIOS 04.02, kernel 7.2, amd-pstate-epp
active. After an eleven-hour s2idle suspend on mains, every core sat at
600-858 MHz against a 5.09 GHz maximum at 95 % busy and 8 W package power,
with temperatures in the forties, no thermal trips, and zero memory pressure.
`scaling_max_freq` was at the hardware maximum, the profile was `balanced`,
and forcing the performance governor, EPP, or profile changed nothing. The
charger replug cleared it. Framework tracks the firmware side of this in
their SoftwareFirmwareIssueTracker (issue 91 and the community threads it
links).

## Cost

The probe is a thread, not a subprocess, so it does not count against the
refresh process budget (`refresh-budget.md`). It blocks a core for 300 ms per
class and runs on `gio::spawn_blocking` alongside the snapd reads that every
discovery already awaits, so it adds nothing to the wall clock of a refresh
and never touches the main thread. There is still no periodic poll: the
number is as old as the last refresh, which the report states.

## Follow-up

The daemon sees every session's realized inference speed and could raise the
same warning from the runtime side, which would catch a clamp that begins
after the Settings page was last opened. That needs a per-session real-time
factor published over the dictation D-Bus interface next to the audio-drop
counts; nothing here precludes it.
