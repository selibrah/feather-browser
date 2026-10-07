//! Parallel downloads, and progress for the ones WebKit runs itself.
//!
//! A single HTTP connection is usually not what limits a download: per-connection shaping,
//! window scaling and distance to the edge all are. Asking for six byte ranges at once and
//! stitching them together is what every download accelerator does, and it is the same idea
//! a segmented streaming player uses to fill its buffer faster.
//!
//! The HTTP client here is `curl`, which ships with macOS and already handles TLS,
//! redirects, proxies and `Range`. That is one fewer crate in the tree and one fewer TLS
//! stack to keep patched.

use log::{info, warn};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How many ranges to ask for at once.
///
/// Six is where the curve flattens for most links: enough to beat per-connection shaping,
/// few enough that a server does not start refusing. ponytail: fixed, not adaptive. Make it
/// adaptive when someone can show a link where a different number is measurably better.
const PARTS: usize = 6;

/// Below this, one connection is already the whole story and the parts cost more than they save.
const MIN_SPLIT: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Update {
    pub name: String,
    pub done: u64,
    pub total: u64,
    /// Bytes per second over the last sample window.
    pub bps: u64,
    /// Bytes fetched so far per parallel range, for the progress display.
    pub parts: Vec<u64>,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Running,
    Done,
    Failed(String),
}

fn user_agent() -> String {
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
     (KHTML, like Gecko) Version/17.0 Safari/605.1.15"
        .to_string()
}

/// Asks the server for the size, and whether it will serve ranges at all.
///
/// A HEAD is the polite question, but plenty of CDNs answer it differently from a GET, so a
/// one-byte range request is the fallback: the `Content-Range` in the reply proves both
/// facts at once.
pub fn probe(url: &str, referer: Option<&str>) -> Option<(u64, bool)> {
    let mut cmd = Command::new("curl");
    cmd.args(["-sIL", "--max-time", "20", "-A", &user_agent()]);
    if let Some(r) = referer {
        cmd.args(["-H", &format!("Referer: {}", r)]);
    }
    cmd.arg(url);
    let head = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&head.stdout).to_lowercase();
    let len = header_value(&text, "content-length").and_then(|v| v.parse::<u64>().ok());
    let ranges = text.contains("accept-ranges: bytes");
    if let Some(total) = len {
        if ranges {
            return Some((total, true));
        }
    }

    // Fall back to asking for one byte and reading what came back.
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sL", "--max-time", "20", "-o", "/dev/null", "-D", "-", "-r", "0-0",
        "-A", &user_agent(),
    ]);
    if let Some(r) = referer {
        cmd.args(["-H", &format!("Referer: {}", r)]);
    }
    cmd.arg(url);
    let probe = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&probe.stdout).to_lowercase();
    if let Some(cr) = header_value(&text, "content-range") {
        // content-range: bytes 0-0/1234567
        if let Some((_, total)) = cr.rsplit_once('/') {
            if let Ok(n) = total.trim().parse::<u64>() {
                return Some((n, true));
            }
        }
    }
    len.map(|n| (n, false))
}

fn header_value(lowercased: &str, name: &str) -> Option<String> {
    lowercased
        .lines()
        .rfind(|l| l.starts_with(name))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

/// Downloads `url` to `dest` using parallel range requests, reporting as it goes.
///
/// Blocking: run it on its own thread. Falls back to a single connection whenever the
/// server will not serve ranges, or the file is too small to be worth splitting.
pub fn download(
    url: &str,
    dest: &Path,
    referer: Option<&str>,
    name: &str,
    report: &dyn Fn(Update),
) {
    let (total, ranges) = probe(url, referer).unwrap_or((0, false));
    let split = ranges && total >= MIN_SPLIT;
    if split && attempt(url, dest, referer, name, total, PARTS, report) {
        return;
    }
    if split {
        // Advertising `Accept-Ranges` is not a promise to serve six at once: plenty of
        // hosts answer the seventh connection with 429 and mean it. One connection always
        // works, and a download that completes slowly beats one that fails quickly.
        warn!("Parallel download of {} was refused; retrying on one connection.", name);
    }
    attempt(url, dest, referer, name, total, 1, report);
}

/// One attempt at the transfer. Returns whether it completed.
#[allow(clippy::too_many_arguments)]
fn attempt(
    url: &str,
    dest: &Path,
    referer: Option<&str>,
    name: &str,
    total: u64,
    parts: usize,
    report: &dyn Fn(Update),
) -> bool {
    let split = parts > 1;
    info!(
        "Downloading {} ({} bytes) over {} connection(s)",
        name, total, parts
    );

    let tmp_dir = dest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());

    let mut children = Vec::new();
    let mut part_paths: Vec<PathBuf> = Vec::new();
    let mut expected: Vec<u64> = Vec::new();
    for i in 0..parts {
        let part_path = tmp_dir.join(format!(".{}.part{}", stem, i));
        let _ = std::fs::remove_file(&part_path);
        let mut cmd = Command::new("curl");
        cmd.args(["-sSL", "--fail", "--max-time", "3600", "-A", &user_agent()]);
        if let Some(r) = referer {
            cmd.args(["-H", &format!("Referer: {}", r)]);
        }
        if split {
            let chunk = total / parts as u64;
            let start = chunk * i as u64;
            let end = if i == parts - 1 { total - 1 } else { start + chunk - 1 };
            cmd.args(["-r", &format!("{}-{}", start, end)]);
            expected.push(end - start + 1);
        }
        cmd.args(["-o", &part_path.to_string_lossy()]);
        cmd.arg(url);
        cmd.stdout(Stdio::null()).stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(child) => {
                children.push(child);
                part_paths.push(part_path);
            }
            Err(e) => {
                report(Update {
                    name: name.to_string(),
                    done: 0,
                    total,
                    bps: 0,
                    parts: vec![],
                    state: State::Failed(format!("could not start curl: {}", e)),
                });
                return true;
            }
        }
    }

    // Progress comes from the part files on disk rather than curl's own meter: it is one
    // stat() per part and it needs no parsing of anyone's output format.
    let mut last_done = 0u64;
    let mut last_at = Instant::now();
    let mut bps = 0u64;
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let sizes: Vec<u64> = part_paths
            .iter()
            .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .collect();
        let done: u64 = sizes.iter().sum();
        let elapsed = last_at.elapsed().as_secs_f64();
        if elapsed >= 0.8 {
            bps = ((done.saturating_sub(last_done)) as f64 / elapsed) as u64;
            last_done = done;
            last_at = Instant::now();
        }

        let mut running = false;
        let mut failure: Option<String> = None;
        for child in children.iter_mut() {
            match child.try_wait() {
                Ok(None) => running = true,
                Ok(Some(status)) if !status.success() => {
                    failure = Some(format!("curl exited with {}", status));
                }
                _ => {}
            }
        }

        if let Some(err) = failure {
            for child in children.iter_mut() {
                let _ = child.kill();
            }
            for p in &part_paths {
                let _ = std::fs::remove_file(p);
            }
            if split {
                // Leave the reporting to the single-connection retry.
                return false;
            }
            warn!("Download of {} failed: {}", name, err);
            report(Update {
                name: name.to_string(),
                done,
                total,
                bps: 0,
                parts: sizes,
                state: State::Failed(err),
            });
            return true;
        }

        report(Update {
            name: name.to_string(),
            done,
            total,
            bps,
            parts: sizes.clone(),
            state: State::Running,
        });

        if !running {
            break;
        }
    }

    // A server may advertise ranges and then ignore them, answering every part with the
    // whole file. Joining those would produce six copies end to end and a corrupt download
    // that nothing else would catch, so the parts are measured before they are joined.
    if split {
        let sizes: Vec<u64> = part_paths
            .iter()
            .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .collect();
        if sizes != expected {
            warn!(
                "Ranges were not honoured for {}: got {:?}, expected {:?}",
                name, sizes, expected
            );
            for p in &part_paths {
                let _ = std::fs::remove_file(p);
            }
            return false;
        }
    }

    match join_parts(&part_paths, dest) {
        Ok(written) => {
            info!("Download of {} finished: {} bytes", name, written);
            report(Update {
                name: name.to_string(),
                done: written,
                total: if total > 0 { total } else { written },
                bps: 0,
                parts: vec![],
                state: State::Done,
            });
        }
        Err(e) => report(Update {
            name: name.to_string(),
            done: 0,
            total,
            bps: 0,
            parts: vec![],
            state: State::Failed(e),
        }),
    }
    true
}

/// Concatenates the parts in order and removes them.
///
/// Streamed in 1 MB blocks rather than read whole: the point of this browser is not holding
/// a film in memory to write it back out again.
pub(crate) fn join_parts(parts: &[PathBuf], dest: &Path) -> Result<u64, String> {
    let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut written = 0u64;
    for p in parts {
        let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            written += n as u64;
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    for p in parts {
        let _ = std::fs::remove_file(p);
    }
    Ok(written)
}

/// Watches a file WebKit is downloading and reports how far along it is.
///
/// wry reports a download starting and finishing and nothing between, but the bytes are
/// landing in a file we chose the path for, so the progress is on disk to be read.
pub fn watch(dest: PathBuf, total: u64, name: String, report: &dyn Fn(Update)) {
    let mut last_done = 0u64;
    let mut last_at = Instant::now();
    let mut idle_for = Duration::ZERO;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let done = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        let elapsed = last_at.elapsed().as_secs_f64();
        let bps = ((done.saturating_sub(last_done)) as f64 / elapsed.max(0.001)) as u64;

        if done == last_done {
            idle_for += Duration::from_millis(500);
        } else {
            idle_for = Duration::ZERO;
        }
        last_done = done;
        last_at = Instant::now();

        if total > 0 && done >= total {
            return;
        }
        // WebKit's completion handler is the authority on success. This loop only stops
        // guessing once nothing has arrived for long enough that it has clearly ended.
        if idle_for > Duration::from_secs(20) {
            return;
        }
        report(Update {
            name: name.clone(),
            done,
            total,
            bps,
            parts: vec![],
            state: State::Running,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parts_join_in_order() {
        // The whole accelerator rests on this: six files written at once must come back as
        // one stream in the order the ranges were asked for.
        let dir = std::env::temp_dir().join(format!("feather-accel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let parts: Vec<PathBuf> = (0..4)
            .map(|i| {
                let p = dir.join(format!("p{}", i));
                std::fs::write(&p, format!("chunk{}-", i).repeat(3)).unwrap();
                p
            })
            .collect();
        let dest = dir.join("joined");
        let written = join_parts(&parts, &dest).unwrap();

        let got = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(
            got,
            "chunk0-chunk0-chunk0-chunk1-chunk1-chunk1-chunk2-chunk2-chunk2-chunk3-chunk3-chunk3-"
        );
        assert_eq!(written, got.len() as u64);
        assert!(!parts[0].exists(), "parts are cleaned up after joining");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Serves a known body and honours `Range`, so the range arithmetic and the join are
    /// tested without depending on anyone's CDN or rate limiter.
    fn range_server(port: u16, honour_ranges: bool) -> std::process::Child {
        let script = format!(
            r#"
import http.server, re, sys
DATA = bytes(range(256)) * 40000
HONOUR = {}
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_HEAD(self): self._send(True)
    def do_GET(self): self._send(False)
    def _send(self, head_only):
        rng = self.headers.get('Range')
        if rng and HONOUR:
            m = re.match(r'bytes=(\d+)-(\d*)', rng)
            a = int(m.group(1)); b = int(m.group(2)) if m.group(2) else len(DATA) - 1
            body = DATA[a:b+1]
            self.send_response(206)
            self.send_header('Content-Range', 'bytes %d-%d/%d' % (a, b, len(DATA)))
        else:
            body = DATA
            self.send_response(200)
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        if not head_only:
            self.wfile.write(body)
http.server.ThreadingHTTPServer(('127.0.0.1', {}), H).serve_forever()
"#,
            if honour_ranges { "True" } else { "False" },
            port
        );
        let child = Command::new("python3")
            .args(["-c", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 is needed for this test");
        std::thread::sleep(Duration::from_millis(700));
        child
    }

    fn expected_body() -> Vec<u8> {
        let mut v = Vec::new();
        for _ in 0..40000 {
            v.extend(0u8..=255u8);
        }
        v
    }

    #[test]
    fn test_parallel_download_reassembles_the_exact_file() {
        let port = 8791;
        let mut server = range_server(port, true);
        let dir = std::env::temp_dir().join(format!("feather-accel-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.bin");

        let widest = std::cell::RefCell::new(0usize);
        let last = std::cell::RefCell::new(None);
        download(
            &format!("http://127.0.0.1:{}/f.bin", port),
            &dest,
            None,
            "f.bin",
            &|u| {
                if u.parts.len() > *widest.borrow() {
                    *widest.borrow_mut() = u.parts.len();
                }
                *last.borrow_mut() = Some(u);
            },
        );
        let _ = server.kill();
        let _ = server.wait();

        assert_eq!(
            last.borrow().as_ref().map(|u| u.state.clone()),
            Some(State::Done),
            "download did not finish"
        );
        assert_eq!(*widest.borrow(), PARTS, "the file was not split across connections");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            expected_body(),
            "the reassembled file differs from what the server served"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_a_server_that_ignores_ranges_still_gets_an_intact_file() {
        // The trap this exists for: a host advertises Accept-Ranges, ignores the Range, and
        // answers every part with the whole body. Joining those gives six copies end to end.
        let port = 8792;
        let mut server = range_server(port, false);
        let dir = std::env::temp_dir().join(format!("feather-accel-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.bin");

        let last = std::cell::RefCell::new(None);
        download(
            &format!("http://127.0.0.1:{}/f.bin", port),
            &dest,
            None,
            "f.bin",
            &|u| { *last.borrow_mut() = Some(u); },
        );
        let _ = server.kill();
        let _ = server.wait();

        assert_eq!(
            last.borrow().as_ref().map(|u| u.state.clone()),
            Some(State::Done),
            "the fallback did not complete the download"
        );
        let got = std::fs::read(&dest).unwrap();
        let want = expected_body();
        assert_eq!(got.len(), want.len(), "file is {} bytes, expected {}", got.len(), want.len());
        assert_eq!(got, want);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_header_value_takes_the_last_one() {
        // curl -L prints the headers of every hop; the size that matters is the final one.
        let headers = "http/1.1 302 found\r\ncontent-length: 0\r\n\r\n\
                       http/1.1 200 ok\r\ncontent-length: 8192\r\naccept-ranges: bytes\r\n";
        assert_eq!(header_value(headers, "content-length"), Some("8192".into()));
        assert_eq!(header_value(headers, "nope"), None);
    }
}
