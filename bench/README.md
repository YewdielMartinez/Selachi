# Selachi benchmark harness

Fase 0 of `docs/PLAN.md` (section 10): real numbers for Selachi vs. Windows Terminal vs. Warp, which
feed **ADR-0001** (the UI stack decision: GPUI vs. the Tauri plan B). The project exists to use far fewer
resources than Warp, so these scripts are the yardstick, not an afterthought.

Requirements: Windows 10/11, PowerShell 7 (`pwsh`). No other dependencies. Run from the repo root.

| Script | Purpose |
|---|---|
| `bench/measure.ps1` | Launch one app N times, measure startup, memory of the whole process tree, idle CPU. Appends to a CSV. |
| `bench/compare.ps1` | Runs `measure.ps1` for Selachi, Windows Terminal and Warp and writes `bench/results/latest.md`. |
| `bench/flood.ps1` | Output throughput test. Run it *inside* the terminal under test. |
| `bench/threads.ps1` | Per-thread CPU of a process over N seconds (thread names via `GetThreadDescription`). Answers "which thread burns CPU at idle?". |
| `bench/bursts.ps1` | Time-resolved per-thread CPU: prints every sampling interval in which a thread burned CPU (and whether the window was focused). Tells periodic work (timers, scans) from one-off bursts. |
| `bench/vmregions.ps1` | VMMap-style breakdown of a process' committed memory (private / image / mapped, biggest private regions). Answers "where do the Private Bytes come from?". |
| `bench/shot.ps1` | Launches an exe, optionally types keys into it, saves a PNG of its window (visual check after render-path changes). |
| `bench/patches/gpui-selachi.patch` | Patch for gpui 0.2.2 (demand-driven vsync thread + lazy path textures); see the perf notes below. |

Output goes to `bench/results/` (`results.csv` = raw rows, `latest.md` = last comparison).

## Quick start

```powershell
cargo build --release                       # produces target/release/selachi.exe
# Close every Windows Terminal / Warp window first (see "Single-instance apps").
pwsh -File bench/compare.ps1                # 3 runs x 30 s idle per terminal -> bench/results/latest.md

# One target only:
pwsh -File bench/measure.ps1 -Exe target/release/selachi.exe -Name selachi -IdleSeconds 30 -Runs 3
pwsh -File bench/measure.ps1 -Exe wt.exe -Name wt -Runs 1 -IdleSeconds 10 -Verbose   # -Verbose = per-process breakdown

# Throughput: type this inside each terminal, same window size every time
pwsh -File bench/flood.ps1                  # 100 MB of ANSI-colored text
pwsh -File bench/flood.ps1 -Plain           # same amount, no escape sequences
```

`measure.ps1 -Exe` accepts a path or an app execution alias (`wt.exe`). Extra arguments: `-Args '--flag','value'`.

### Idle CPU and the usage monitor

`crates/app/src/usage_monitor.rs` ingests the agents' logs only while the window is focused and only for files a notify watcher marked as changed. A benchmark launched from a script is usually *not* the foreground window, so the scan path would never run; set `SELACHI_BENCH_USAGE_ACTIVE=1` in the environment of the process under test to make the monitor behave as if focused (worst case for scans, real `~/.claude` and `~/.codex` logs). Use an isolated `SELACHI_DATA_DIR` for config/state; the usage database stays in `%APPDATA%\Selachi`.

## What `measure.ps1` measures

Per run: launch the app, wait for the window, wait a 5 s settle period (children such as WebView2,
OpenConsole and the shell appear late), then sample the whole process tree once per second during
`-IdleSeconds`. Nothing is typed or clicked: this is the **idle** cost, the number that matters for a
terminal that sits open all day.

| Column (CSV) | Meaning |
|---|---|
| `startup_ms` | Launch (just before `Start-Process`) until the first **visible, un-cloaked, top-level window** owned by a process of the tree (EnumWindows by PID). Resolution is about 10-20 ms (the CIM process-table refresh is skipped during the first 1.5 s of the poll, otherwise it stalled the loop for 100-300 ms and inflated every startup number). It is "first window visible", not "first prompt painted"; compare it with a screen recording if you care about the difference. |
| `private_mb_total` / `private_mb_app` | **Private Bytes** (committed private memory, `Win32_Process.PrivatePageCount`). The honest number: it is not inflated by shared DLLs. Median over the idle samples. |
| `ws_mb_total` / `ws_mb_app` | **Working Set**: physical pages resident, *including pages shared between processes* (each Chromium/WebView2 process counts the shared DLLs again, so sums over-state). It is what Task Manager's "Memory" column roughly shows; reported because people compare it. |
| `cpu_idle_pct` | Average CPU of the tree over the idle window as % of **one core** (100 = one full core). Computed from per-process `UserModeTime + KernelModeTime` deltas, so processes that start or exit during the window are accounted for. |
| `cpu_idle_pct_allcores` | Same value divided by the logical processor count (this is what Task Manager shows). |
| `cpu_idle_pct_app` | Same as `cpu_idle_pct`, excluding the shell. |
| `process_count` / `process_count_app` | Processes in the tree at the last sample. |

**total vs. app.** *total* is the entire tree started by the launch. *app* excludes the shell (`pwsh.exe`,
`powershell.exe`, `cmd.exe`, `bash.exe`, `wsl.exe`, ... and everything below them), so that two terminals
are compared on their own cost and not on whichever shell profile you happen to have. Terminal-owned
helpers (`OpenConsole.exe`, `conhost.exe`, `msedgewebview2.exe`, GPU/crash helpers) **are** counted
in *app*. Override the shell list with `-ShellNames`.

The summary table printed at the end (and `latest.md`) shows the **median** of the runs.
`-Verbose` prints the per-process breakdown (name, PID, parent, shell flag, WS, private, CPU seconds)
of the last sample of every run.

### How the process tree is found

The tracked set is the PID that `Start-Process` returned, plus every descendant (CIM `Win32_Process.ParentProcessId`,
re-queried continuously), plus processes of known hand-off images that did not exist before launch:
`wt.exe` -> `WindowsTerminal.exe`, `warp.exe` -> `warp.exe`, `notepad.exe` -> `notepad.exe`. Add more with
`-AdoptName foo.exe`, or `-SingleInstance` to also adopt the exe's own image name.

### Safety: it only stops what it started

* Processes are stopped **by PID only**, and only PIDs in the tracked set (PID + start time are
  re-validated against PID reuse). The script first sends a polite window close, then `Stop-Process -Id`
  on survivors. It never uses `Stop-Process -Name`.
* **Single-instance apps.** If `WindowsTerminal.exe`, `warp.exe` (or any `-SingleInstance` target)
  is already running when a run starts, the script prints a warning, does not touch that instance and skips
  the run. Close the app manually and re-run. This is also why a run on your daily-driver Windows Terminal
  needs it closed: otherwise the new window would attach to the existing process and nothing could be measured fairly.
* Residual risk: adoption-by-name is only active while launching (until the window appears + 1.5 s). If you open
  that same application yourself during that second, it could be adopted and closed at the end.

## `flood.ps1`: output throughput

Prints N MB (default 100) of pre-generated, deterministic (`-Seed`) mixed ANSI text (16/256/truecolor,
bold/dim/underline, resets) as raw bytes and reports MB/s. It measures how fast the *terminal* consumes
output (ConPTY pipe -> VT parser -> grid -> render coalescing), i.e. the "cat a 100 MB file" risk in `PLAN.md` section 9,
not the speed of PowerShell. `-Plain` removes every escape sequence: the difference between the two runs is the
VT-parsing cost. The reported time is when the terminal has accepted the last byte (back-pressure from the pipe makes
this track the terminal's consumption speed); a terminal may need an extra few hundred ms to paint the tail.
For a fair comparison use the same window size, no transparency/blur, three runs and the median.
`-ResultFile path.txt` also appends the result line to a file (handy for scripting).

Interesting secondary checks while flooding: Selachi must stay responsive (Ctrl+C returns at once) and its
memory must stay bounded by the scrollback limit; run `measure.ps1` on a second window afterwards to see the
residual memory.

## Input latency: manual step (not automated)

Keystroke-to-pixel latency cannot be measured reliably from inside a script because it needs the real display
pipeline. Use an external tool, the same way for every terminal:

1. **Typometer** (https://pavelfatin.com/typometer/): run it, point it at the terminal window, use a
   maximised window of identical size, same font and size, vsync/GPU settings left at defaults. It injects keystrokes and
   detects the resulting pixel change. Take at least 200 samples per terminal and record mean / p99.
2. Alternative: **`is-it-snappy`** (search for the project; verify its Windows support) or a 240 fps phone camera pointed at the screen
   (count frames between key press and glyph).
3. Record the result by hand in `bench/results/latency.md` (terminal, tool, window size, mean, p99, date).

Run with the terminal idle and nothing else heavy on the machine; plug in the laptop and use the "best performance" power mode.

## Reproducibility checklist

* Plugged in, "best performance" power mode, nothing else running (close browsers, IDEs, other terminals).
* Release builds only (`cargo build --release`); Warp and Windows Terminal at the versions shown in `latest.md`.
* Same shell profile for everything (or compare *app* numbers, which exclude the shell).
* Same window size/font; do not touch the machine during a run.
* 3 runs minimum; the first run after a reboot or update is a cold start and is usually slower: discard or note it.
* Warp is not a pure terminal: it may sign in, sync or check for updates in the background. Its first launch shows a
  login screen unless already signed in; say so when quoting numbers.

## How this feeds ADR-0001

ADR-0001 (UI stack) decides GPUI vs. the Tauri 2 + xterm.js plan B using the spike's numbers (`PLAN.md` section 1 lists the
order-of-magnitude expectations as "a medir"). Procedure:

1. Build the spike (`cargo build --release`), close all Windows Terminal/Warp instances.
2. `pwsh -File bench/compare.ps1` and keep `bench/results/latest.md` + `results.csv` in the ADR.
3. Run `flood.ps1` (ANSI and plain) inside each terminal; add the lines to the ADR.
4. Measure input latency manually (above).
5. Decision rule, to agree before looking at the numbers: Selachi **private (app)** memory and idle CPU clearly below
   Warp and not worse than Windows Terminal beyond a tolerance; startup in the same range; flood throughput without
   stalls. If GPUI misses by a wide margin on Windows, trigger plan B; otherwise record GPUI as confirmed.

The same scripts are the basis for the CI bench of Fase 1 (flood and idle thresholds as regressions).

## Known limitations

* Startup = first visible window, not first rendered prompt or input readiness.
* The shell can still be loading its profile when idle sampling starts (default settle: 5 s); that is why *app* CPU is the fairer figure.
* CPU times come from the kernel's per-process counters (scheduler-tick granularity, about 15 ms). Deltas are summed over the window, so values below ~0.1 % of a core are noise; use longer windows for very low CPU.
* Windows only; needs permission to enumerate processes (normal user is enough for the user's own processes).
* A visible window belonging to the tree is required; apps that start tray-only report `startup_ms` empty after the timeout.
* `Export-Csv -Append` requires the CSV header to match; delete or rename an old `results.csv` after a column change.
