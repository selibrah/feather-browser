//! Memory measurement.
//!
//! The number that matters is not this process's RSS. WKWebView renders out of process:
//! `com.apple.WebKit.WebContent`, `.Networking` and `.GPU` are XPC services reparented to
//! launchd, so they are invisible to `task_info` and are not our children. Measuring only
//! the engine process would produce a flattering number that says nothing about what the
//! browser costs the machine.
//!
//! So there are two figures here, and both are reported: what the engine itself holds, and
//! what the WebKit processes hold on top of it.

use log::warn;
use std::process::Command;

/// Resident set size of this process, in bytes.
// libc deprecated its mach bindings in favour of the `mach2` crate. These two calls are the
// only ones the project needs and they are stable kernel interfaces, so this takes the
// warning rather than a dependency.
#[allow(deprecated)]
#[cfg(target_os = "macos")]
pub fn engine_rss() -> Option<u64> {
    use std::mem;

    let mut info: libc::mach_task_basic_info = unsafe { mem::zeroed() };
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;

    // Safety: task_info fills `info` with `count` u32-sized words; the count constant is
    // derived from the struct itself, so the buffer cannot be too small.
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as libc::task_info_t,
            &mut count,
        )
    };

    if result == libc::KERN_SUCCESS {
        Some(info.resident_size)
    } else {
        warn!("task_info failed with {}", result);
        None
    }
}

#[cfg(not(target_os = "macos"))]
pub fn engine_rss() -> Option<u64> {
    // /proc/self/statm reports pages; field 2 is resident.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident * 4096)
}

/// Total physical memory on the machine, in bytes.
#[cfg(target_os = "macos")]
pub fn total_ram() -> Option<u64> {
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();

    // Safety: hw.memsize is a u64 sysctl; `size` tells the kernel how much room it has.
    let result = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            &mut value as *mut _ as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };

    (result == 0).then_some(value)
}

#[cfg(not(target_os = "macos"))]
pub fn total_ram() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// Combined resident size of every WebKit XPC process on the machine, in bytes.
///
/// Every process, not just ours — there is no public API that maps a `WKWebView` to the pid
/// rendering it, and the XPC services are reparented to launchd the moment they start. The
/// caller subtracts a baseline taken before the first tab existed, which is accurate on an
/// idle machine and an over-count on one running Safari.
// ponytail: shells out to ps rather than walking proc_listpids/proc_pidpath/proc_pid_rusage
// by hand. This is sampled a handful of times per run, not in a loop.
pub fn webkit_rss() -> u64 {
    let output = match Command::new("ps").args(["-Ao", "rss=,comm="]).output() {
        Ok(o) => o,
        Err(e) => {
            warn!("Could not run ps to measure WebKit processes: {}", e);
            return 0;
        }
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("com.apple.WebKit"))
        .filter_map(|line| line.split_whitespace().next()?.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .sum()
}

/// Bytes as a short human-readable string, for logs and tables.
pub fn mib(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// Whether `--profile` was passed. A process-wide flag rather than a field threaded through
/// every constructor: it is read-only after startup and only ever gates a log line.
static PROFILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    PROFILING.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn enabled() -> bool {
    PROFILING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Logs a memory reading, if profiling is on. `what` says what just happened.
pub fn snapshot(what: &str, live_views: usize, total_tabs: usize) {
    if !enabled() {
        return;
    }
    let engine = engine_rss().unwrap_or(0);
    let webkit = webkit_rss();
    log::info!(
        "[profile] {:<22} tabs {:>2} ({} live) · engine {:>9} · webkit {:>9} · total {:>9}",
        what,
        total_tabs,
        live_views,
        mib(engine),
        mib(webkit),
        mib(engine + webkit),
    );
}

/// Pages the bench opens, in order. Real pages, because the cost of a tab is the cost of
/// what is in it — a synthetic blank page would measure nothing worth knowing.
pub const BENCH_URLS: [&str; 10] = [
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://news.ycombinator.com/",
    "https://doc.rust-lang.org/book/",
    "https://www.bbc.com/news",
    "https://github.com/tauri-apps/wry",
    "https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API",
    "https://www.theguardian.com/international",
    "https://crates.io/",
    "https://stackoverflow.com/questions",
    "https://example.com/",
];

/// One row of the bench table.
struct BenchRow {
    tabs: usize,
    live: usize,
    engine: u64,
    webkit: u64,
}

/// Opens tabs one at a time, letting each settle, and records what the machine paid.
///
/// The WebKit figure is a delta against a baseline taken before the first tab, because
/// `webkit_rss` cannot tell our content processes from anyone else's. Run it on an idle
/// machine with no other WebKit app open, or the number is someone else's Safari.
pub struct Bench {
    tabs_to_open: usize,
    opened: usize,
    settle: std::time::Duration,
    next_at: std::time::Instant,
    baseline_webkit: u64,
    rows: Vec<BenchRow>,
}

/// What the event loop should do at this tick.
pub enum BenchStep {
    /// Nothing due yet.
    Wait,
    /// Open this page as a new tab.
    Open(&'static str),
    /// Every tab is open and measured; print and quit.
    Finish,
}

impl Bench {
    /// `settle_secs` is how long a page gets to load and allocate before it is measured.
    pub fn new(tabs_to_open: usize, settle_secs: u64) -> Self {
        let settle = std::time::Duration::from_secs(settle_secs);
        Self {
            tabs_to_open: tabs_to_open.min(BENCH_URLS.len()),
            opened: 0,
            settle,
            // The first tab is opened by the normal startup path, so the baseline has to be
            // taken before the event loop runs at all.
            next_at: std::time::Instant::now() + settle,
            baseline_webkit: webkit_rss(),
            rows: Vec::new(),
        }
    }

    pub fn next_at(&self) -> std::time::Instant {
        self.next_at
    }

    /// Called from the event loop. Records the current state, then says what to do next.
    pub fn tick(&mut self, live_views: usize, total_tabs: usize) -> BenchStep {
        if std::time::Instant::now() < self.next_at {
            return BenchStep::Wait;
        }

        self.rows.push(BenchRow {
            tabs: total_tabs,
            live: live_views,
            engine: engine_rss().unwrap_or(0),
            webkit: webkit_rss().saturating_sub(self.baseline_webkit),
        });

        self.next_at = std::time::Instant::now() + self.settle;

        if self.opened >= self.tabs_to_open {
            return BenchStep::Finish;
        }

        let url = BENCH_URLS[self.opened];
        self.opened += 1;
        BenchStep::Open(url)
    }

    /// Prints the table. Straight to stdout, not the log — it is the output of the run.
    pub fn report(&self) {
        let ram = total_ram().map(mib).unwrap_or_else(|| "unknown".into());
        println!("\nFeatherBrowser memory profile");
        println!("machine RAM {} · live-view ceiling {}", ram, crate::tabs::max_active_views());
        println!("WebKit column is a delta against the processes running before launch.\n");
        println!("{:>5}  {:>5}  {:>11}  {:>11}  {:>11}", "tabs", "live", "engine", "webkit", "total");
        println!("{}", "-".repeat(52));
        for row in &self.rows {
            println!(
                "{:>5}  {:>5}  {:>11}  {:>11}  {:>11}",
                row.tabs,
                row.live,
                mib(row.engine),
                mib(row.webkit),
                mib(row.engine + row.webkit),
            );
        }
        if let (Some(first), Some(last)) = (self.rows.first(), self.rows.last()) {
            let added_tabs = last.tabs.saturating_sub(first.tabs);
            if added_tabs > 0 {
                let added = (last.engine + last.webkit).saturating_sub(first.engine + first.webkit);
                println!("\n{} more tabs cost {} — {} per tab.", added_tabs, mib(added), mib(added / added_tabs as u64));
            }
        }
        println!();
    }
}
