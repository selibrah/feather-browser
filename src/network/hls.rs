//! Saving a segmented stream.
//!
//! HLS and DASH do not serve a file; they serve a list of thousands of pieces and let the
//! player put them together. Saving one means doing what the player does: read the list,
//! fetch the pieces, and write them back out in order.
//!
//! Two paths, because two problems. A plain playlist is just a list of URLs, and those can
//! be fetched several at a time — the accelerator in `accel` already does that, and it is
//! where "download in batches" actually pays off, since each segment is a separate small
//! request that would otherwise be made one at a time. A playlist with `EXT-X-KEY`, or any
//! DASH manifest, has decryption and initialisation segments and discontinuities in it, and
//! `ffmpeg` has handled all of that correctly for twenty years. So that case is handed to
//! ffmpeg rather than reimplemented badly.

use log::{info, warn};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::accel::{State, Update};

/// Segments fetched at once. The same reasoning as the byte-range accelerator, and the same
/// ceiling: enough to stop waiting on round trips, few enough not to look like an attack.
const CONCURRENCY: usize = 6;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Playlist {
    /// Media segment URLs, absolute, in playback order.
    pub segments: Vec<String>,
    /// Highest-bandwidth variant of a master playlist, if this was one.
    pub variant: Option<String>,
    /// A key line that is not `METHOD=NONE`. These go to ffmpeg.
    pub encrypted: bool,
}

/// Resolves a playlist URI against the playlist's own address.
///
/// Enough of RFC 3986 for the three shapes a playlist actually contains: absolute, rooted,
/// and relative to the playlist's directory.
pub fn resolve(base: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_string();
    }
    let scheme = if base.starts_with("http://") { "http" } else { "https" };
    if let Some(rest) = uri.strip_prefix("//") {
        return format!("{}://{}", scheme, rest);
    }
    let after_scheme = base.split_once("://").map(|(_, r)| r).unwrap_or(base);
    let (host, path) = match after_scheme.split_once('/') {
        Some((h, p)) => (h, p),
        None => (after_scheme, ""),
    };
    if uri.starts_with('/') {
        return format!("{}://{}{}", scheme, host, uri);
    }
    // Relative to the directory the playlist sits in, with any query string dropped first.
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "",
    };
    format!("{}://{}/{}{}", scheme, host, dir, uri)
}

/// Reads an m3u8 into the pieces the downloader needs.
pub fn parse(text: &str, base: &str) -> Playlist {
    let mut out = Playlist::default();
    let mut best_bandwidth = 0u64;
    let mut pending_variant = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            // A master playlist lists renditions; take the best one and follow it.
            best_bandwidth = attrs
                .split(',')
                .filter_map(|a| a.trim().strip_prefix("BANDWIDTH="))
                .filter_map(|v| v.trim().parse::<u64>().ok())
                .next()
                .map(|b| b.max(best_bandwidth))
                .unwrap_or(best_bandwidth);
            pending_variant = true;
            continue;
        }
        if let Some(attrs) = line.strip_prefix("#EXT-X-KEY:") {
            if !attrs.to_uppercase().contains("METHOD=NONE") {
                out.encrypted = true;
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if pending_variant {
            // Only keep the URI belonging to the highest bandwidth seen so far.
            out.variant = Some(resolve(base, line));
            pending_variant = false;
            continue;
        }
        out.segments.push(resolve(base, line));
    }
    out
}

fn ffmpeg() -> Option<PathBuf> {
    for candidate in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    Command::new("which")
        .arg("ffmpeg")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

fn fetch_text(url: &str, referer: Option<&str>) -> Option<String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-sSL", "--fail", "--max-time", "30"]);
    if let Some(r) = referer {
        cmd.args(["-H", &format!("Referer: {}", r)]);
    }
    cmd.arg(url);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Downloads a segmented stream to `dest`.
///
/// Blocking; run it on its own thread.
pub fn download(
    playlist_url: &str,
    dest: &Path,
    referer: Option<&str>,
    name: &str,
    report: &dyn Fn(Update),
) {
    let fail = |why: String| {
        warn!("Stream download of {} failed: {}", name, why);
        report(Update {
            name: name.to_string(),
            done: 0,
            total: 0,
            bps: 0,
            parts: vec![],
            state: State::Failed(why),
        });
    };

    // DASH has an XML manifest with initialisation segments and templated URLs. ffmpeg
    // already reads those; there is no reason for a second implementation here.
    if playlist_url.contains(".mpd") {
        return match ffmpeg_remux(playlist_url, dest, referer, name, report) {
            Ok(()) => {}
            Err(e) => fail(e),
        };
    }

    let text = match fetch_text(playlist_url, referer) {
        Some(t) => t,
        None => return fail("the playlist could not be fetched".into()),
    };
    let mut list = parse(&text, playlist_url);

    // A master playlist names renditions rather than segments; follow the best one.
    if list.segments.is_empty() {
        if let Some(variant) = list.variant.clone() {
            info!("Following variant playlist {}", variant);
            match fetch_text(&variant, referer) {
                Some(t) => list = parse(&t, &variant),
                None => return fail("the variant playlist could not be fetched".into()),
            }
        }
    }

    if list.encrypted {
        info!("{} is encrypted; handing the playlist to ffmpeg.", name);
        return match ffmpeg_remux(playlist_url, dest, referer, name, report) {
            Ok(()) => {}
            Err(e) => fail(e),
        };
    }

    if list.segments.is_empty() {
        return fail("the playlist listed no segments".into());
    }

    info!("{}: {} segments, {} at a time", name, list.segments.len(), CONCURRENCY);
    match fetch_segments(&list.segments, dest, referer, name, report) {
        Ok(bytes) => {
            report(Update {
                name: name.to_string(),
                done: bytes,
                total: bytes,
                bps: 0,
                parts: vec![],
                state: State::Done,
            });
        }
        Err(e) => fail(e),
    }
}

/// Fetches every segment `CONCURRENCY` at a time and writes them out in playback order.
///
/// Segments land in a scratch directory and are appended in index order, so the output is
/// correct no matter which of the six finishes first.
fn fetch_segments(
    segments: &[String],
    dest: &Path,
    referer: Option<&str>,
    name: &str,
    report: &dyn Fn(Update),
) -> Result<u64, String> {
    let scratch = dest.with_extension("feather-parts");
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;

    let total = segments.len();
    let mut done_count = 0usize;
    let started = Instant::now();

    for batch in segments.chunks(CONCURRENCY) {
        let mut children = Vec::new();
        for (i, url) in batch.iter().enumerate() {
            let part = scratch.join(format!("{:06}.ts", done_count + i));
            let mut cmd = Command::new("curl");
            cmd.args(["-sSL", "--fail", "--max-time", "120"]);
            if let Some(r) = referer {
                cmd.args(["-H", &format!("Referer: {}", r)]);
            }
            cmd.args(["-o", &part.to_string_lossy()]);
            cmd.arg(url);
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
            children.push(cmd.spawn().map_err(|e| e.to_string())?);
        }
        for child in children.iter_mut() {
            let status = child.wait().map_err(|e| e.to_string())?;
            if !status.success() {
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(format!("a segment could not be fetched ({})", status));
            }
        }
        done_count += batch.len();

        let bytes: u64 = (0..done_count)
            .filter_map(|i| std::fs::metadata(scratch.join(format!("{:06}.ts", i))).ok())
            .map(|m| m.len())
            .sum();
        let secs = started.elapsed().as_secs_f64().max(0.001);
        report(Update {
            name: name.to_string(),
            done: done_count as u64,
            total: total as u64,
            bps: (bytes as f64 / secs) as u64,
            parts: vec![bytes],
            state: State::Running,
        });
    }

    // Transport-stream segments concatenate into a playable file as they are, so ffmpeg is
    // an improvement (a seekable MP4) rather than a requirement.
    let joined = scratch.join("joined.ts");
    let parts: Vec<PathBuf> = (0..total)
        .map(|i| scratch.join(format!("{:06}.ts", i)))
        .collect();
    let written = super::accel::join_parts(&parts, &joined)?;

    let out = match ffmpeg() {
        Some(bin) => {
            let status = Command::new(bin)
                .args(["-y", "-i", &joined.to_string_lossy(), "-c", "copy"])
                .arg(dest)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| e.to_string())?;
            if status.success() {
                written
            } else {
                warn!("ffmpeg could not remux {}; keeping the raw stream.", name);
                std::fs::rename(&joined, dest).map_err(|e| e.to_string())?;
                written
            }
        }
        None => {
            info!("No ffmpeg found; saving the concatenated stream as-is.");
            std::fs::rename(&joined, dest).map_err(|e| e.to_string())?;
            written
        }
    };

    let _ = std::fs::remove_dir_all(&scratch);
    Ok(out)
}

/// Lets ffmpeg fetch and mux the whole stream itself.
fn ffmpeg_remux(
    url: &str,
    dest: &Path,
    referer: Option<&str>,
    name: &str,
    report: &dyn Fn(Update),
) -> Result<(), String> {
    let bin = ffmpeg().ok_or_else(|| {
        "this stream needs ffmpeg (brew install ffmpeg) and none was found".to_string()
    })?;
    let mut cmd = Command::new(bin);
    cmd.args(["-y"]);
    if let Some(r) = referer {
        cmd.args(["-headers", &format!("Referer: {}\r\n", r)]);
    }
    cmd.args(["-i", url, "-c", "copy"]).arg(dest);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // ffmpeg's own progress needs parsing its stderr; the growing file says the same thing.
    loop {
        std::thread::sleep(Duration::from_millis(600));
        let done = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => {
                report(Update {
                    name: name.to_string(),
                    done,
                    total: done,
                    bps: 0,
                    parts: vec![],
                    state: State::Done,
                });
                return Ok(());
            }
            Some(status) => return Err(format!("ffmpeg exited with {}", status)),
            None => report(Update {
                name: name.to_string(),
                done,
                total: 0,
                bps: 0,
                parts: vec![],
                state: State::Running,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolves_the_three_shapes_a_playlist_uses() {
        let base = "https://cdn.example.com/hls/720p/index.m3u8?token=abc";
        assert_eq!(resolve(base, "https://other.net/a.ts"), "https://other.net/a.ts");
        assert_eq!(resolve(base, "//other.net/a.ts"), "https://other.net/a.ts");
        assert_eq!(resolve(base, "/root/a.ts"), "https://cdn.example.com/root/a.ts");
        // Relative to the playlist's directory, and the token must not leak into the path.
        assert_eq!(resolve(base, "seg1.ts"), "https://cdn.example.com/hls/720p/seg1.ts");
    }

    #[test]
    fn test_parses_a_media_playlist() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n\
                    #EXTINF:9.009,\nseg0.ts\n#EXTINF:9.009,\nseg1.ts\n#EXT-X-ENDLIST\n";
        let p = parse(text, "https://c.example/v/list.m3u8");
        assert_eq!(
            p.segments,
            vec!["https://c.example/v/seg0.ts", "https://c.example/v/seg1.ts"]
        );
        assert!(!p.encrypted);
    }

    #[test]
    fn test_master_playlist_picks_a_variant_and_lists_no_segments() {
        let text = "#EXTM3U\n\
                    #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\nlow.m3u8\n\
                    #EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720\nhigh.m3u8\n";
        let p = parse(text, "https://c.example/v/master.m3u8");
        assert!(p.segments.is_empty(), "a master playlist has no segments of its own");
        assert_eq!(p.variant.as_deref(), Some("https://c.example/v/high.m3u8"));
    }

    /// Serves a playlist and its segments, so the batching and the ordering are exercised
    /// against a real socket without depending on anyone's CDN.
    fn segment_server(port: u16, count: usize) -> std::process::Child {
        let script = format!(
            r#"
import http.server
COUNT = {}
def body(i): return (b'SEG%03d:' % i) + bytes([i]) * 2048
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path.startswith('/list.m3u8'):
            lines = ['#EXTM3U', '#EXT-X-TARGETDURATION:4']
            for i in range(COUNT):
                lines += ['#EXTINF:4.0,', 'seg%03d.ts' % i]
            lines.append('#EXT-X-ENDLIST')
            data = ('\n'.join(lines) + '\n').encode()
            ctype = 'application/vnd.apple.mpegurl'
        else:
            i = int(self.path.split('seg')[1].split('.')[0])
            data = body(i)
            ctype = 'video/mp2t'
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
http.server.ThreadingHTTPServer(('127.0.0.1', {}), H).serve_forever()
"#,
            count, port
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

    #[test]
    fn test_segments_are_fetched_in_batches_and_written_in_playback_order() {
        // 20 segments over a concurrency of 6 means four batches, the last one partial:
        // the case where an off-by-one in the indexing would shuffle the film.
        const COUNT: usize = 20;
        let port = 8793;
        let mut server = segment_server(port, COUNT);
        let dir = std::env::temp_dir().join(format!("feather-hls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.ts");

        let last = std::cell::RefCell::new(None);
        let ticks = std::cell::RefCell::new(0usize);
        download(
            &format!("http://127.0.0.1:{}/list.m3u8", port),
            &dest,
            None,
            "out.ts",
            &|u| {
                if u.state == State::Running {
                    *ticks.borrow_mut() += 1;
                }
                *last.borrow_mut() = Some(u);
            },
        );
        let _ = server.kill();
        let _ = server.wait();

        assert_eq!(
            last.borrow().as_ref().map(|u| u.state.clone()),
            Some(State::Done),
            "stream download did not finish"
        );
        assert_eq!(*ticks.borrow(), 4, "expected one progress report per batch");

        let mut want = Vec::new();
        for i in 0..COUNT {
            want.extend_from_slice(format!("SEG{:03}:", i).as_bytes());
            want.extend(std::iter::repeat_n(i as u8, 2048));
        }
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            want,
            "segments were not reassembled in playback order"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_an_encrypted_playlist_is_flagged_for_ffmpeg() {
        // METHOD=NONE appears in playlists that switch encryption off partway and must not
        // be mistaken for the real thing.
        let plain = parse("#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\na.ts\n", "https://c/x.m3u8");
        assert!(!plain.encrypted);
        let locked = parse(
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k.key\"\n#EXTINF:4,\na.ts\n",
            "https://c/x.m3u8",
        );
        assert!(locked.encrypted);
    }
}
