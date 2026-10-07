# FeatherBrowser

A low-memory browser engine in Rust, built on [wry](https://github.com/tauri-apps/wry) and
[winit](https://github.com/rust-windowing/winit). One process, a hard ceiling on live
WebViews, and everything else hibernated to SQLite.

## What works today

**Deterministic tab suspension.** A tab is its state — URL, scroll position, title, parent —
and the WebView is a disposable rendering resource. `TabState::Active(WebView)` versus
`TabState::Dormant(TabSnapshot)`. When the number of live views would exceed
`MAX_ACTIVE_VIEWS`, the least recently used tab is snapshotted to `sessions.db` and its
WebView is dropped. Switching back rebuilds a fresh WebView and restores the scroll
position. The concept is Min browser's; the hard LRU ceiling is stricter than Min's
inactivity timeout.

**Sessions that survive a restart.** Every open tab is persisted to `sessions.db` — id, URL,
title, scroll position, tree parent and form contents — and restored at launch. Restored
tabs come back dormant, so a large session costs one WebView at startup regardless of how
many tabs it holds; the tab that was in front is the one that gets rendered. Launching with
a URL restores the session *and* opens that URL, so `feather <url>` never costs you the
session. Tab ids continue past the last session instead of restarting at 1 and overwriting
it.

**Form state.** What you typed comes back with the tab, whether it was hibernated or the
browser was restarted. Password fields, hidden fields and `autocomplete="off"` fields are
never collected — this file is not a keychain. Field contents are dropped as soon as the tab
navigates away, so they cannot be replayed into an unrelated page with the same field names.

**A measured memory ceiling.** The live-WebView limit is derived from the machine's RAM —
one view per 4 GB, clamped to 2–8, overridable with `FEATHER_MAX_ACTIVE_VIEWS`. `--profile`
logs a reading on every tab event; `--bench=N` opens N real pages, measures each step and
prints a table. Numbers below.

**Omnibar and history.** URL/query detection, DuckDuckGo bang shortcuts (`!g`, `!gh`, `!w`,
`!yt`, `!cr`, `!docs`), and suggestions ranked by visit count and recency out of SQLite.

**Client runtime.** A zero-dependency init script per tab: Vim-style scrolling and `f`-key
link hinting (Vimium), a CSS-filter dark mode (Dark Reader's Filter mode), cookie-banner
removal, and fixation-point bolding. It tries not to take the page over:

- **Vim keys are off until you turn them on, per site** (the ⌨ button). Bare `j`/`f`/`t`/`x`
  swallow keystrokes globally, which breaks every site that has its own shortcuts. The
  editable check follows `role="combobox"`, `searchbox` and `spinbutton`, `contenteditable`
  ancestors, and focus into shadow roots, so text fields keep their keys even when enabled.
- **Dark mode is remembered per origin** and reapplied on load instead of being lost at the
  next navigation.
- **Single-page apps are tracked.** `pushState`, `replaceState`, `popstate` and `hashchange`
  are watched, so the tab strip follows a route change and a hibernated tab rehydrates to
  the URL you were actually on.
- **The banner sweep is throttled.** It runs once before the next paint on a dirty flag,
  as a single combined selector — a thousand DOM mutations cost one document scan rather
  than sixteen thousand. The `[aria-label*="cookie"]` selector is gone; it deleted real
  content on any page that discussed cookies.

**The things that make it a browser.** Back and forward per tab (`Cmd+[` / `Cmd+]`, or
`H`/`L` with vim keys on), find-in-page (`Cmd+F` or `/`), page zoom (`Cmd+=` / `Cmd+-` /
`Cmd+0`), a right-click menu on links with open-in-new-tab and copy-link, downloads with a
native save panel and a status row, favicons in the tab strip, bookmarks, and a link-hover
status line. The find bar lives in a shadow root and takes no focus — `window.find` searches
the rendered document, so a focused query field would match the text you had just typed into
it, and stepping between matches only works while the matches themselves hold the selection.
Right-clicking anything that is not a link leaves WebKit's own menu alone.

**Bookmarks are history rows with a flag.** A bookmark is a page you visited, the omnibar
already ranks that table, and starring a page you have not visited yet creates the row. They
sort ahead of plain history in the omnibar and carry their own badge.

**Navigation-level privacy.** Tracking parameters (`utm_*`, `fbclid`, `gclid`, `msclkid`,
`mc_eid`, `_ga`) are stripped from every navigation, and requests matching the filter list
are denied before the page loads.

**Subresource blocking.** EasyList and EasyPrivacy are converted to WebKit's
content-blocking format and compiled by `WKContentRuleListStore`, so blocking happens inside
WebKit's networking layer and covers every request a page makes, not just the navigation.
A current pair of lists yields ~108,000 rules, split across several lists because WebKit
caps the size of one. Compiled lists are cached on disk by a content-derived identifier, so
the several-second compile happens once per list version rather than once per launch. The
shield in the topbar shows the live rule count.

Run `scripts/fetch-filter-lists.sh` once to install the lists; without them the browser
starts normally and logs that subresource blocking is off.

Audited by loading eight ad-heavy sites and collecting every subresource each page actually
requested: cnn.com, forbes.com, dailymail.co.uk, weather.com, espn.com, speedtest.net and
imdb.com loaded 25–160 subresources apiece and **not one of them came from an ad or
analytics host**. Directly requested, `ad.doubleclick.net`, `adsbygoogle.js`,
`analytics.js`, `gtm.js`, `scorecardresearch` and `adnxs` are all refused, while
`example.com` loads — and a real `<script>` tag pointed at adsbygoogle fires `onerror`.
That audit was also the entry gate for a MITM proxy, which needed three sites where this
blocking demonstrably failed. Not one could be named, so the proxy stays unbuilt and no
certificate authority goes into anyone's trust store.

### Video controls on any page's player

`Alt+V` (or the ⚡ button) opens a panel over whatever video the page is playing: speed,
loop, picture-in-picture, volume boost past 100%, a three-band equaliser, a compressor, a
mono downmix, brightness/contrast/saturation, and a local `.srt` or `.vtt` subtitle file
with size and colour controls. Without opening anything, `Alt+←/→` seeks, `Alt+↑/↓` sets
volume, `Alt+[` and `Alt+]` change speed, `Alt+,` and `Alt+.` step a frame, and `Alt+P`,
`Alt+L`, `Alt+M` toggle picture-in-picture, loop and mute.

The video it acts on is whichever one is playing, or the largest one if none is — looked up
per keystroke, so a player a script swaps in is found without watching the DOM.

**It follows the video into iframes.** On a streaming site the `<video>` is almost never in
the page you navigated to; it is inside a cross-origin player frame the top page cannot even
read. WebKit gives an init script to the main frame only, so the video layer ships as a
second script injected into every frame, and a frame with no video of its own forwards the
command down to its children by `postMessage` — recursively, since these players nest. Only
commands from the embedding frame are accepted. Verified on a real site: the panel opened
inside the player frame and `Alt+→` / `Alt+]` moved that video, two frames and two origins
away from the page in the address bar.

The audio half only appears when the media is same-origin, `blob:`/MSE, or has opted into
CORS. `createMediaElementSource` on a cross-origin element silences it permanently and
disconnecting does not undo that, so on a page whose video comes from a CDN the panel says
so and offers picture and subtitles only. Verified both ways: same-origin file gets the
equaliser, the same file served from a second port gets the note.

### Built for watching things

Four changes aimed at sites whose whole job is a player, measured against one of them
rather than guessed at:

**A tab playing a video is never the one hibernated.** The memory ceiling suspends the
least recently *accessed* tab, which is an exact description of a film playing in a
background tab — so the browser was stopping the thing you were watching in order to save
memory you never asked it to save. Playing tabs are now taken last, and only when every
live tab is playing, so the ceiling is still a ceiling.

**The tab strip shows which tab is making noise**, and the speaker mutes it. The video is
usually several frames below the page, so each frame reports its own playback state to its
parent and the top frame sends the total to the host — the same route the controls take
going down.

**It remembers where you stopped.** Position is stored on the history row for the page, so
a 46-minute episode picks up where it left off. Only between one and two minutes from
either end: below that there is nothing to resume, and near the end you have finished. It
seeks only if playback has just begun, so scrubbing somewhere yourself is never undone.

**Picture-in-picture from a player frame needs the keystroke to land in that frame.**
`Alt+P` typed while the player has focus works. Forwarded from the top frame it does not:
WebKit requires user activation for picture-in-picture and a `postMessage` does not carry
activation across the frame boundary. Nothing can carry it — click the player first.

What was checked and did **not** need building: popups and popunders (none fired on load
or on click — the content rules already stop them), display sleep during playback (WebKit
takes `PreventUserIdleDisplaySleep` itself), and a decluttered cinema mode (the player
already fills the whole content area).

### Seeing what the stream is doing

`Alt+S`, or the 📶 button, opens a live monitor over the player: a timeline of every range
the player actually holds against the length of the film, the buffer-ahead line over the
last two minutes with stalls shaded behind it, and a readout of resolution, requests in
flight, rebuffer count, dropped frames and adaptive-bitrate shifts. It reads the same APIs
the player uses — `buffered`, `getVideoPlaybackQuality`, Resource Timing — so it works on
any video, in whichever frame the video lives.

Measured on a real stream: 53s of buffer held, 1920×960 detected, 3 frames dropped out of
672. Throughput reads `n/a` there, because Resource Timing zeroes `transferSize` for
cross-origin responses without `Timing-Allow-Origin`, and no browser can invent that number.

### Downloads: progress, and parallel when it helps

Downloads now show a percentage, a bar, a rate and an ETA. wry reports a download starting
and finishing and nothing between, but the bytes land in a file whose path we chose, so the
progress is read off the file with one `stat` per tick, and the total comes from a separate
`HEAD`.

Accelerated download — six byte ranges at once, reassembled in order — is available from
the video panel for a direct media URL. The HTTP client is `curl`, which ships with macOS
and already handles TLS, redirects and `Range`; that is one fewer crate and one fewer TLS
stack to keep patched.

**It is an explicit action, not the default, because it is not always faster.** Measured
against a Debian mirror on a 60 MB range: one connection 8.1s, six connections 9.3s — a
0.87× *slowdown*, because the link was already saturated by a single stream and splitting
only bought six TLS handshakes. Parallel ranges win when the *server* shapes per connection,
not when your own line is the limit. Two failure modes found by measuring and handled: a
host that answers the sixth connection with `429` (OVH does) falls back to one connection,
and a host that advertises `Accept-Ranges` then ignores the `Range` — answering every part
with the whole body — is caught by checking each part against the length that was asked for,
because joining those would produce a corrupt file that nothing else would notice.

### Making the player hold more

A streaming page's player decides how far ahead it keeps, and hls.js ships a thirty-second
default that nothing on a streaming site ever raises. Measured on a live player here, the
buffer sat between 11 and 14 seconds the whole time it played. **Deep buffer** in the video
panel reaches that player's own config and moves it: 180 seconds ahead, a 200 MB byte cap
instead of 60, and the back buffer trimmed to 30 seconds.

The back buffer is the part people miss. MSE gives each source a byte quota, so ten minutes
of already-watched video sitting behind the playhead is quota the player cannot spend in
front of it. Trimming it is how the deeper target becomes reachable rather than aspirational.

Nothing here fetches anything. The site's player makes exactly the requests it was already
making, just further in front of you — which is why this works on streams that **Fetch
ahead** and **Save stream** cannot touch: it never needs to read the playlist.

Finding the player is the actual work. Loaded as a global script, it is sitting on `window`.
Bundled by webpack — every Next.js player is — it exposes nothing at all, so the search walks
React's fiber off the video element and looks down the hook list for the engine. If the
library is loaded but no instance is reachable, the constructor's defaults get seeded and the
setting lands the next time the player builds one, which on a streaming page is constantly.
Turning it off puts every value back.

`node scripts/check-video.js` covers those shapes: a hook ref, a wrapper object, a global, a
dash.js instance, a constructor with no instance, and a bare video that is not a player.

### Fetching ahead of the player

A segmented player draws a sawtooth: fetch a piece, drain it, fetch the next. That is not a
bandwidth problem, it is a waiting problem — the next request does not start until the last
one finished. **Fetch ahead** in the video panel runs two minutes in front of playback, six
segments at a time, so the pieces are already in the HTTP cache when the player asks.

It deliberately does **not** replace the player. MSE cannot accept MPEG-TS, so feeding the
element directly would mean shipping a transmuxer, and taking over playback would mean
reimplementing the bitrate switching and error recovery the site already has. Priming the
cache leaves all of that alone. The fetched bytes are not kept — holding a film in memory is
the thing this browser exists not to do.

Measured against a deliberately slow origin (30 segments, 1.2s of latency injected into
every one), same clip, both runs on a cold cache:

| | Average buffer | Peak | Shape |
|---|---|---|---|
| Player alone | 9.5s | 17.0s | sawtooth, dips on every segment |
| Fetching ahead | **32.7s** | **49.3s** | climbs, then drains smoothly |

3.4× the average buffer, and the sawtooth is gone: after the first eight seconds the buffer
falls by exactly one second per second, which is what "never waiting on the network" looks
like. 30 of 30 segments primed, no misses.

Same gate as saving: if the playlist is not readable by page script, there is nothing to
fetch ahead, and the panel says so rather than pretending.

### Saving a segmented stream

HLS and DASH do not serve a file, they serve a list of pieces, so saving one means doing
what the player does: read the list, fetch the pieces, write them back in order. The video
panel offers **Download stream** when the source is a playlist, and segments are fetched six
at a time — which is where batching genuinely pays, since each segment is its own small
request that would otherwise be made one after another.

Two paths, because there are two problems. A plain playlist is a list of URLs and Feather
handles it directly. A playlist carrying `EXT-X-KEY`, or any DASH manifest, has decryption,
initialisation segments and discontinuities in it, and `ffmpeg` has done that correctly for
twenty years — so that case is handed to ffmpeg rather than reimplemented badly. Transport
segments concatenate into a playable file on their own, so ffmpeg is an upgrade to a
seekable MP4, not a requirement; without it the raw stream is saved and still plays.

**Whether a stream can be saved is the site's decision, not a list kept here.** The button
appears only if the playlist is readable by page script. A site that blocks script access to
its stream has said no, and that answer stands — there is no header-forging path around it.
Verified both ways: a local HLS stream shows the button, and a site that refuses script
access to its playlist shows the refusal instead.

## What does not work yet

**Deep buffer knows hls.js and dash.js, and nothing else.** A site running its own MSE engine
— obfuscated player bundles sometimes do — has no buffer setting to find, and the panel says
so rather than claiming a win it did not get.

**Blocking on non-macOS.** The content-rule bridge is `WKContentRuleList`, which is Apple's.
On Linux and Windows `src/platform/unsupported.rs` compiles a no-op and only the
navigation-level filter applies.

**The first launch after a list update.** Converting EasyList takes a few seconds and a few
hundred megabytes, and a page opened during it can issue requests before blocking starts.
Every launch after that reuses what WebKit compiled and attaches rules before the first page
has loaded.

**No blocked-request count.** WebKit's rule engine does not report individual blocked
requests back to the host, so the topbar shows how many rules are live, not how many
requests they stopped. There is no honest way to show the latter without a proxy.

**Form restore is shallow.** Fields are matched by `name` or `id`, and the value is assigned
followed by `input` and `change` events. That covers ordinary forms. It will not restore a
contenteditable body, a field with neither name nor id, or a component that keeps its value
somewhere other than the DOM node.

**The find bar is a find bar, not Chrome's.** `window.find` reports whether it matched and
nothing else, so there is no "3 of 17" count and no highlight of the other matches. The
query field takes no focus, which is what makes stepping work, and the cost is that there is
no caret, no paste and no IME in it — keystrokes and backspace.

**Favicons are guessed from `/favicon.ico`.** The tab strip asks each site's origin for its
icon rather than reading the `<link rel=icon>` the page declares. Three of four real sites
answer; the fourth (rust-lang.org, which declares an SVG) gets the globe it had before.

**Frame stepping assumes 30 fps.** Nothing in the DOM reports a video's real frame rate, so
`Alt+,` and `Alt+.` move by 1/30 s. On 24 or 60 fps content that is not exactly one frame.

**A panel opened in an iframe is clipped by that iframe.** The controls render inside the
frame holding the video, so on a player embedded in a small box the panel is bounded by that
box. Full-size players — which is most of them — are unaffected.

**The video panel's settings are per-visit.** Speed, EQ and picture adjustments are not
remembered for a site, and the panel is not a player: there is no seek bar, no subtitle
search, and no parallel-fragment prebuffering.

**Zoom is not persisted:** a hibernated tab comes back at 100%.

**The stream monitor cannot show throughput on most streams.** `transferSize` is zero for
cross-origin responses that do not send `Timing-Allow-Origin`, which is nearly all of them,
so the rate reads `n/a` while every other figure stays live.

**Stream saving picks the highest bitrate and takes it.** A master playlist's renditions are
not offered as a choice; the best one is followed. Nothing resumes a part-finished stream
either — an interrupted download starts again.

**Fetching ahead is off by default and per-visit.** It spends bandwidth in front of
playback, which is the wrong trade on a metered connection, so it is a switch rather than a
policy — and it is not remembered between pages.

**Live streams are not handled.** The downloader reads a playlist once and fetches what it
lists, so a rolling live window saves only the segments present at that moment.

## What it costs

Measured with `--bench=10` on an 18 GB machine (ceiling: 4 live views), release build, real
pages, idle machine. WebKit renders out of process — `WebContent`, `Networking` and `GPU`
are XPC services reparented to launchd — so the engine's own RSS is only part of the bill
and both halves are reported. The WebKit column is a delta against the processes running
before launch.

| tabs | live views | engine | WebKit | total |
|---:|---:|---:|---:|---:|
| 0 | 0 | 96.4 MB | 92.4 MB | **188.8 MB** |
| 1 | 1 | 100.3 MB | 264.7 MB | 365.0 MB |
| 2 | 2 | 100.9 MB | 303.7 MB | 404.7 MB |
| 4 | 4 | 102.8 MB | 579.1 MB | 681.9 MB |
| 6 | 4 | 106.3 MB | 715.4 MB | 821.7 MB |
| 8 | 4 | 103.6 MB | 493.1 MB | 596.8 MB |
| 10 | 4 | 104.3 MB | 472.8 MB | **577.1 MB** |

The engine process is flat: tabs cost WebKit memory, not ours. And past the ceiling the
total stops climbing and comes back down — ten tabs cost less than six, because six tabs
means four live views and six means four live views, with the rest hibernated. That curve is
the entire thesis of the project, and it is the first time it has been measured rather than
asserted.

The original spec claimed under 120 MB with five tabs open. That was never true and is not
true now: five tabs cost about 760 MB all in. What is true is that the number stops growing.

Instrumenting this found a 230 MB bug. Converting EasyList to WebKit's format allocated
several hundred megabytes that the allocator never returned to the OS, and it was happening
on *every* launch — the engine sat at 327 MB before a single page loaded. WebKit already
keeps compiled rule lists in its own on-disk store, so a `content-rules.json` manifest now
records the identifiers and a warm launch asks for them directly, never touching a filter
list. Baseline went from 326.9 MB to 96.4 MB, and rules now attach fast enough that blocking
is live before the first page finishes loading.

See [ROADMAP.html](ROADMAP.html) for the plan and the order.

## Build

Requires Rust 1.80+. On Linux, `libwebkit2gtk-4.1-dev`.

State lives in `~/Library/Application Support/FeatherBrowser` (`$XDG_DATA_HOME/FeatherBrowser`
elsewhere): `sessions.db` and the filter lists. Not the working directory — which session you
got used to depend on where you launched the binary from.

```
./scripts/fetch-filter-lists.sh   # once: installs EasyList + EasyPrivacy
cargo run                      # restores the last session, or opens example.com
cargo run -- news.ycombinator.com
cargo run -- "rust wry tutorial"   # non-URL input goes to DuckDuckGo
cargo test
cargo run --release -- --bench=10  # memory table
cargo run -- --profile             # log a reading on every tab event
```

## Keys

| Key | Action |
|---|---|
| `Cmd+T` / `t` | New tab |
| `Cmd+W` / `x` | Close tab |
| `Cmd+L` / `o` | Omnibar |
| `Cmd+1..9` | Switch to tab by position |
| `Ctrl+Tab` | Cycle tabs |
| `j` `k` `d` `u` `gg` `G` | Scroll |
| `f` | Link hints |
| `Cmd+[` / `Cmd+]` / `H` / `L` | Back / forward |
| `Cmd+F` / `/` | Find in page (`⏎` next, `⇧⏎` previous, `esc` close) |
| `Cmd+=` / `Cmd+-` / `Cmd+0` | Zoom in / out / reset |
| `Cmd+Ctrl+F` / `F11` | Full screen |
| `Alt+V` | Video panel |
| `Alt+S` | Stream health monitor |
| `Alt+←` / `Alt+→` | Seek 5s |
| `Alt+↑` / `Alt+↓` | Volume (above 100% boosts) |
| `Alt+[` / `Alt+]` / `Alt+\` | Speed down / up / reset |
| `Alt+,` / `Alt+.` | Step a frame |
| `Alt+P` / `Alt+L` / `Alt+M` | Picture-in-picture / loop / mute |

Single-letter keys (`j` `k` `d` `u` `gg` `G` `f` `o` `t` `x` `H` `L` `/`) only work on sites you have
enabled with the ⌨ button; `Cmd`-prefixed shortcuts always work. The setting is stored per
origin in `sessions.db`.
