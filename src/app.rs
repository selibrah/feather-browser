use crate::network::{rules, HistoryMatch, SearchEngine, TrafficFilter, UrlCleaner};
use crate::platform::ContentBlocker;
use crate::tabs::{generate_restore_script, origin_of, SitePref, TabError, TabSnapshot, TabStore};
use log::{debug, info, warn};
use serde::Deserialize;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalSize};
use winit::window::Window;
use wry::{Rect, WebView, WebViewBuilder};

static RUNTIME_SCRIPT: &str = include_str!("scriptlets/runtime.js");
// Injected into every frame, not just the main one: on a streaming site the <video> lives
// in a cross-origin player iframe, which is precisely where runtime.js cannot go.
static VIDEO_SCRIPT: &str = include_str!("scriptlets/video.js");
static TOPBAR_HTML: &str = include_str!("assets/chrome/topbar.html");

/// Fixed vertical height in points for the chrome topbar.
pub const TOPBAR_HEIGHT: f64 = 40.0;

/// Inter-process communication payload from client runtime JavaScript.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientIpcMessage {
    #[serde(rename = "scroll_update")]
    ScrollUpdate {
        #[serde(rename = "scrollX")]
        scroll_x: i32,
        #[serde(rename = "scrollY")]
        scroll_y: i32,
        title: Option<String>,
        url: Option<String>,
        /// Field values the page volunteered, as a JSON object. Persisted verbatim.
        #[serde(rename = "formData", default)]
        form_data: Option<serde_json::Value>,
    },
    #[serde(rename = "navigate")]
    Navigate { url: String },
    #[serde(rename = "new_tab")]
    NewTab {
        url: Option<String>,
        parent_id: Option<u64>,
    },
    #[serde(rename = "switch_tab")]
    SwitchTab { id: u64 },
    #[serde(rename = "switch_tab_index")]
    SwitchTabIndex { index: usize },
    #[serde(rename = "close_tab")]
    CloseTab { id: u64 },
    #[serde(rename = "close_current_tab")]
    CloseCurrentTab,
    #[serde(rename = "next_tab")]
    NextTab,
    #[serde(rename = "prev_tab")]
    PrevTab,
    #[serde(rename = "open_omnibar")]
    OpenOmnibar,
    #[serde(rename = "omnibar_query")]
    OmnibarQuery { query: String },
    #[serde(rename = "toggle_dark_mode")]
    ToggleDarkMode,
    /// The page is asking what this origin has turned on. Sent at load and whenever an SPA
    /// navigates across origins.
    #[serde(rename = "site_prefs_request")]
    SitePrefsRequest,
    #[serde(rename = "toggle_vim_keys")]
    ToggleVimKeys,
    #[serde(rename = "set_site_pref")]
    SetSitePref { key: SitePref, value: bool },
    #[serde(rename = "toggle_bionic")]
    ToggleBionic,
    /// Step through this tab's own session history. `history.back()` inside the page is
    /// the same stack the back button would walk, and it costs no new platform API.
    #[serde(rename = "history_back")]
    HistoryBack,
    #[serde(rename = "history_forward")]
    HistoryForward,
    /// -1 out, +1 in, 0 back to 100%.
    #[serde(rename = "zoom")]
    Zoom { step: i32 },
    #[serde(rename = "toggle_bookmark")]
    ToggleBookmark,
    #[serde(rename = "open_find")]
    OpenFind,
    #[serde(rename = "toggle_video_panel")]
    ToggleVideoPanel,
    #[serde(rename = "toggle_fullscreen")]
    ToggleFullscreen,
    #[serde(rename = "media_state")]
    MediaState { playing: bool, muted: bool },
    #[serde(rename = "media_progress")]
    MediaProgress { position: i64, duration: i64 },
    #[serde(rename = "toggle_tab_muted")]
    ToggleTabMuted { id: u64 },
    #[serde(rename = "toggle_stream_panel")]
    ToggleStreamPanel,
    #[serde(rename = "download_media")]
    DownloadMedia { url: String, referer: Option<String> },
}

/// Internal application message routed from IPC closures to main event loop.
#[derive(Debug)]
pub enum AppEvent {
    TabIpc { tab_id: u64, raw_json: String },
    ChromeIpc { raw_json: String },
    /// A navigation was denied so it can be re-issued with tracking parameters removed.
    /// The navigation handler can only allow or deny, so rewriting has to bounce
    /// through the host and come back as a fresh load.
    Redirect { tab_id: u64, url: String },
    /// A download started, finished or failed. Only these two moments are observable:
    /// wry surfaces no byte counter, so the topbar row is a state, not a percentage.
    Download { name: String, state: DownloadState },
    DownloadProgress { name: String, done: u64, total: u64, bps: u64 },
    /// WebKit finished compiling the content rules. Tabs built before this arrives have
    /// no rule list attached yet, so the event loop attaches it to them now.
    ContentRulesReady,
}

#[derive(Debug, Clone, Copy)]
pub enum DownloadState {
    Started,
    Finished,
    Failed,
}

impl DownloadState {
    fn as_str(self) -> &'static str {
        match self {
            DownloadState::Started => "start",
            DownloadState::Finished => "ok",
            DownloadState::Failed => "fail",
        }
    }
}

/// The filename a URL suggests, for seeding the save panel.
fn suggested_filename(url: &str) -> String {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .filter(|seg| seg.len() < 200)
        .unwrap_or("download")
        .to_string()
}

/// Clustered dependencies for creating a WebView without exceeding argument limits.
#[derive(Clone)]
pub struct WebViewContext {
    pub filter: Rc<TrafficFilter>,
    pub cleaner: UrlCleaner,
    pub blocker: ContentBlocker,
    pub size: PhysicalSize<u32>,
    pub scale_factor: f64,
    pub tx: Sender<AppEvent>,
}

/// Core application state engine for FeatherBrowser.
pub struct BrowserCore {
    pub tab_store: TabStore<WebView>,
    pub traffic_filter: Rc<TrafficFilter>,
    pub url_cleaner: UrlCleaner,
    pub search_engine: SearchEngine,
    pub blocker: ContentBlocker,
    pub current_size: PhysicalSize<u32>,
    pub scale_factor: f64,
    pub topbar_webview: Option<WebView>,
    pub event_sender: Sender<AppEvent>,
    pub event_receiver: Receiver<AppEvent>,
    /// Per-tab zoom, live only. A tab that hibernates comes back at 100%.
    // ponytail: not persisted. Per-origin zoom belongs in site_prefs if anyone asks.
    zoom_levels: std::collections::HashMap<u64, f64>,
}

impl BrowserCore {
    /// Initializes BrowserCore with default network filters and SQLite tab store.
    pub fn new() -> Result<Self, TabError> {
        let (tx, rx) = channel();
        let tab_store = TabStore::new_default()?;
        let traffic_filter = Rc::new(TrafficFilter::new());
        let url_cleaner = UrlCleaner::new();
        let search_engine = SearchEngine::default();

        // WebKit compiles the rules on its own schedule; `ContentRulesReady` reports back.
        let blocker = ContentBlocker::new();
        Self::start_blocking(&blocker, tx.clone());

        Ok(Self {
            tab_store,
            traffic_filter,
            url_cleaner,
            search_engine,
            blocker,
            current_size: PhysicalSize::new(1280, 800),
            scale_factor: 1.0,
            topbar_webview: None,
            event_sender: tx,
            event_receiver: rx,
            zoom_levels: std::collections::HashMap::new(),
        })
    }

    /// Gets subresource blocking going.
    ///
    /// Converting EasyList costs a few hundred megabytes of resident memory that the
    /// allocator does not hand back, so the fast path never does it: WebKit already holds
    /// the compiled lists, and the manifest holds the identifiers needed to ask for them.
    /// Reading the source lists here is only to confirm they have not changed.
    fn start_blocking(blocker: &ContentBlocker, tx: Sender<AppEvent>) {
        let list_dir = rules::default_list_dir();
        let texts = rules::load_lists(&list_dir);

        if texts.is_empty() {
            warn!(
                "No subresource blocking: no filter lists found. Put easylist.txt and \
                 easyprivacy.txt in {} (scripts/fetch-filter-lists.sh does this) and restart.",
                list_dir.display()
            );
            return;
        }

        let source_hash = rules::hash_sources(&texts);

        // Building is the fallback, and the closure owns the list text so the fast path can
        // drop it immediately.
        let rebuild = {
            let dir = list_dir.clone();
            Rc::new(move || match rules::build(&rules::load_lists(&dir)) {
                Ok(compiled) => {
                    rules::save_manifest(
                        &dir,
                        &rules::Manifest {
                            source_hash: rules::hash_sources(&rules::load_lists(&dir)),
                            chunks: compiled
                                .chunks
                                .iter()
                                .map(|c| (c.identifier.clone(), c.rule_count))
                                .collect(),
                        },
                    );
                    Some(compiled)
                }
                Err(e) => {
                    warn!("No subresource blocking: {}", e);
                    None
                }
            }) as Rc<dyn Fn() -> Option<rules::ContentRules>>
        };

        match rules::load_manifest(&list_dir) {
            Some(manifest) if manifest.source_hash == source_hash => {
                info!(
                    "Reusing {} rules WebKit already compiled; skipping the filter list conversion.",
                    manifest.rule_count()
                );
                drop(texts);
                blocker.start_from_manifest(manifest.chunks, tx, rebuild);
            }
            _ => {
                drop(texts);
                if let Some(compiled) = rebuild() {
                    blocker.start(compiled, tx);
                }
            }
        }
    }

    fn webview_context(&self) -> WebViewContext {
        WebViewContext {
            filter: Rc::clone(&self.traffic_filter),
            cleaner: self.url_cleaner.clone(),
            blocker: self.blocker.clone(),
            size: self.current_size,
            scale_factor: self.scale_factor,
            tx: self.event_sender.clone(),
        }
    }

    /// Initializes the topbar chrome UI as a child view of the window.
    pub fn init_topbar(&mut self, window: &Window) -> Result<(), TabError> {
        let logical = self.current_size.to_logical::<f64>(self.scale_factor);
        let bounds = Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(logical.width, TOPBAR_HEIGHT).into(),
        };

        let tx = self.event_sender.clone();
        let topbar = WebViewBuilder::new()
            .with_bounds(bounds)
            .with_html(TOPBAR_HTML)
            .with_ipc_handler(move |req| {
                let body = req.body().to_string();
                let _ = tx.send(AppEvent::ChromeIpc { raw_json: body });
            })
            .build_as_child(window)
            .map_err(|e| TabError::RehydrationFailed(format!("Failed to build topbar: {:?}", e)))?;

        self.topbar_webview = Some(topbar);
        self.sync_tabs_to_topbar();
        // Rules may already be compiled — the topbar is built after BrowserCore::new.
        self.sync_shield_to_topbar();
        Ok(())
    }

    /// Pushes current tab state to the topbar chrome webview.
    pub fn sync_tabs_to_topbar(&self) {
        if let Some(topbar) = &self.topbar_webview {
            let summaries = self.tab_store.get_tab_summaries();
            if let Ok(json) = serde_json::to_string(&summaries) {
                let script = format!("if (window.__updateTabs) {{ window.__updateTabs({}); }}", json);
                let _ = topbar.evaluate_script(&script);
            }
        }
        // The star belongs to whatever is in front, which is exactly what just changed.
        self.sync_bookmark_to_topbar();
    }

    /// Pushes a tab's origin preferences into the page.
    ///
    /// The page asks rather than the host guessing: a tab's origin changes under us on every
    /// link click and every SPA route, and the page is the one that knows when.
    pub fn send_site_prefs(&self, tab_id: u64) {
        let Some(tab) = self.tab_store.get_tab(tab_id) else {
            return;
        };
        let prefs = self.tab_store.site_prefs(&origin_of(&tab.url));
        let Ok(json) = serde_json::to_string(&prefs) else {
            return;
        };
        if let Some(wv) = tab.state.as_active() {
            let _ = wv.evaluate_script(&format!(
                "if (window.__FeatherEngine) {{ window.__FeatherEngine.applySitePrefs({}); }}",
                json
            ));
        }
    }

    /// Tells the topbar how many blocking rules WebKit has live.
    ///
    /// Rule count, not a per-request tally: WKContentRuleList does the blocking inside
    /// WebKit's networking layer and reports nothing back to us, so a "42 ads blocked"
    /// counter would be a number we made up.
    pub fn sync_shield_to_topbar(&self) {
        if let (Some(topbar), true) = (&self.topbar_webview, self.blocker.is_ready()) {
            let script = format!(
                "if (window.__updateShield) {{ window.__updateShield({}); }}",
                self.blocker.rule_count()
            );
            let _ = topbar.evaluate_script(&script);
        }
    }

    /// Creates a new tab with the target URL, enforcing the live-WebView ceiling.
    pub fn create_tab(
        &mut self,
        window: &Window,
        raw_url: &str,
        parent_id: Option<u64>,
    ) -> Result<u64, TabError> {
        let resolved_url = self.search_engine.resolve_query_or_url(raw_url);
        let cleaned_url = self.url_cleaner.clean_url(&resolved_url);
        info!("Creating tab: {} (resolved: {}, cleaned: {})", raw_url, resolved_url, cleaned_url);

        // Hide currently active webview before showing the new one
        if let Some(active_tab) = self.tab_store.get_active_tab_mut() {
            if let Some(wv) = active_tab.state.as_active_mut() {
                let _ = wv.set_visible(false);
            }
        }

        let next_id = self.tab_store.peek_next_id();

        let webview = self
            .build_webview(window, next_id, &cleaned_url, None)
            .map_err(|e| TabError::RehydrationFailed(e.to_string()))?;

        let tab_id = self.tab_store.insert_active_tab(
            "New Tab".into(),
            cleaned_url,
            parent_id,
            webview,
        )?;

        info!(
            "Tab #{} created. Active views: {}, Dormant: {}",
            tab_id,
            self.tab_store.active_tab_count(),
            self.tab_store.dormant_tab_count()
        );

        self.sync_tabs_to_topbar();
        crate::profile::snapshot(
            &format!("opened tab #{}", tab_id),
            self.tab_store.active_tab_count(),
            self.tab_store.total_tab_count(),
        );
        Ok(tab_id)
    }

    /// Rebuilds the last session's tabs and renders the one that was in front.
    ///
    /// Returns how many tabs came back. Every restored tab starts dormant, so the cost of
    /// a 40-tab session at launch is 40 HashMap entries and one WebView.
    pub fn restore_session(&mut self, window: &Window) -> Result<usize, TabError> {
        let front = self.tab_store.restore_session()?;
        let count = self.tab_store.total_tab_count();

        if let Some(id) = front {
            // switch_to_tab rehydrates a dormant tab, which is what every restored tab is.
            // Restore leaves no active tab, so the "already focused" early-return can't fire.
            self.switch_to_tab(window, id)?;
        }

        self.sync_tabs_to_topbar();
        Ok(count)
    }

    /// Switches focus to a specific tab, rehydrating it from SQLite if it was dormant.
    pub fn switch_to_tab(&mut self, window: &Window, target_id: u64) -> Result<(), TabError> {
        if self.tab_store.active_tab_id() == Some(target_id) {
            return Ok(());
        }

        // Hide current active tab
        if let Some(cur) = self.tab_store.get_active_tab_mut() {
            if let Some(wv) = cur.state.as_active_mut() {
                let _ = wv.set_visible(false);
            }
        }

        let is_dormant = self
            .tab_store
            .get_tab(target_id)
            .map(|t| t.state.is_dormant())
            .ok_or(TabError::NotFound(target_id))?;

        if is_dormant {
            info!("Tab #{} is dormant. Rehydrating from SQLite...", target_id);
            let ctx = self.webview_context();

            self.tab_store.rehydrate_tab(target_id, |snapshot| {
                Self::construct_webview(&ctx, window, snapshot.id, &snapshot.url, Some(snapshot))
                    .map_err(|e| TabError::RehydrationFailed(e.to_string()))
            })?;
        } else {
            self.tab_store.focus_tab(target_id)?;
        }

        // Show target tab webview
        if let Some(tab) = self.tab_store.get_active_tab_mut() {
            if let Some(wv) = tab.state.as_active_mut() {
                let _ = wv.set_visible(true);
            }
        }

        info!(
            "Switched to Tab #{}. Total active: {}, Dormant: {}",
            target_id,
            self.tab_store.active_tab_count(),
            self.tab_store.dormant_tab_count()
        );
        crate::profile::snapshot(
            &format!("switched to #{}", target_id),
            self.tab_store.active_tab_count(),
            self.tab_store.total_tab_count(),
        );

        self.sync_tabs_to_topbar();
        Ok(())
    }

    /// Closes a tab and cleans its database entry.
    pub fn close_tab(&mut self, id: u64) -> Result<(), TabError> {
        self.tab_store.close_tab(id)?;
        // Make newly active tab visible if any
        if let Some(active) = self.tab_store.get_active_tab_mut() {
            if let Some(wv) = active.state.as_active_mut() {
                let _ = wv.set_visible(true);
            }
        }
        self.sync_tabs_to_topbar();
        Ok(())
    }

    /// Closes the currently active tab, creating a default tab if none remain.
    pub fn close_current_tab(&mut self, window: &Window) -> Result<(), TabError> {
        if let Some(current_id) = self.tab_store.active_tab_id() {
            self.close_tab(current_id)?;
            if self.tab_store.total_tab_count() == 0 {
                self.create_tab(window, "https://example.com", None)?;
            }
        }
        Ok(())
    }

    /// Cycles focus to the next tab in circular order.
    pub fn cycle_next_tab(&mut self, window: &Window) -> Result<(), TabError> {
        if let Some(next_id) = self.tab_store.next_tab_id() {
            self.switch_to_tab(window, next_id)?;
        }
        Ok(())
    }

    /// Cycles focus to the previous tab in circular order.
    pub fn cycle_prev_tab(&mut self, window: &Window) -> Result<(), TabError> {
        if let Some(prev_id) = self.tab_store.prev_tab_id() {
            self.switch_to_tab(window, prev_id)?;
        }
        Ok(())
    }

    /// Switches to tab at index `index` (0-based, for Cmd+1..9).
    pub fn switch_tab_by_index(&mut self, window: &Window, index: usize) -> Result<(), TabError> {
        if let Some(tab_id) = self.tab_store.get_tab_id_by_index(index) {
            self.switch_to_tab(window, tab_id)?;
        }
        Ok(())
    }

    /// Handles window resize and display scale changes, updating bounds of chrome and active webviews.
    pub fn handle_resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.current_size = new_size;
        self.scale_factor = scale_factor;
        let logical = new_size.to_logical::<f64>(scale_factor);

        let topbar_bounds = Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(logical.width, TOPBAR_HEIGHT).into(),
        };

        let content_height = (logical.height - TOPBAR_HEIGHT).max(0.0);
        let content_bounds = Rect {
            position: LogicalPosition::new(0.0, TOPBAR_HEIGHT).into(),
            size: LogicalSize::new(logical.width, content_height).into(),
        };

        if let Some(topbar) = &self.topbar_webview {
            let _ = topbar.set_bounds(topbar_bounds);
        }

        if let Some(active_tab) = self.tab_store.get_active_tab_mut() {
            if let Some(wv) = active_tab.state.as_active_mut() {
                let _ = wv.set_bounds(content_bounds);
            }
        }
    }

    /// Processes all pending IPC messages sent by WebViews.
    pub fn process_pending_events(&mut self, window: &Window) {
        while let Ok(event) = self.event_receiver.try_recv() {
            match event {
                AppEvent::TabIpc { tab_id, raw_json } => {
                    self.dispatch_ipc_message(window, Some(tab_id), &raw_json);
                }
                AppEvent::ChromeIpc { raw_json } => {
                    self.dispatch_ipc_message(window, None, &raw_json);
                }
                AppEvent::ContentRulesReady => {
                    for webview in self.tab_store.active_webviews() {
                        self.blocker.apply_to(webview);
                    }
                    info!(
                        "Attached {} content rules to {} live tab(s).",
                        self.blocker.rule_count(),
                        self.tab_store.active_tab_count()
                    );
                    self.sync_shield_to_topbar();
                }
                AppEvent::Download { name, state } => {
                    info!("Download {}: {}", state.as_str(), name);
                    if let Some(topbar) = &self.topbar_webview {
                        let script = format!(
                            "if (window.__download) {{ window.__download({}, {}); }}",
                            serde_json::to_string(&name).unwrap_or_else(|_| "\"\"".into()),
                            serde_json::to_string(state.as_str()).unwrap_or_else(|_| "\"\"".into()),
                        );
                        let _ = topbar.evaluate_script(&script);
                    }
                }
                AppEvent::DownloadProgress { name, done, total, bps } => {
                    if let Some(topbar) = &self.topbar_webview {
                        let script = format!(
                            "if (window.__downloadProgress) {{ window.__downloadProgress({}, {}, {}, {}); }}",
                            serde_json::to_string(&name).unwrap_or_else(|_| "\"\"".into()),
                            done, total, bps,
                        );
                        let _ = topbar.evaluate_script(&script);
                    }
                }
                AppEvent::Redirect { tab_id, url } => {
                    if let Some(tab) = self.tab_store.get_tab_mut(tab_id) {
                        if let Some(wv) = tab.state.as_active_mut() {
                            if let Err(e) = wv.load_url(&url) {
                                warn!("Failed to re-issue cleaned navigation to {}: {:?}", url, e);
                            }
                        }
                    }
                }
            }
        }
    }

    fn dispatch_ipc_message(&mut self, window: &Window, tab_id: Option<u64>, raw_json: &str) {
        let msg: ClientIpcMessage = match serde_json::from_str(raw_json) {
            Ok(m) => m,
            Err(e) => {
                debug!("Failed to deserialize IPC message: {:?}, raw: {}", e, raw_json);
                return;
            }
        };

        match msg {
            ClientIpcMessage::ScrollUpdate {
                scroll_x,
                scroll_y,
                title,
                url,
                form_data,
            } => {
                if let Some(id) = tab_id {
                    let had_title = title.is_some();
                    let had_url = url.is_some();
                    if let Some(ref u) = url {
                        let t = title.as_deref().unwrap_or("");
                        let _ = self.tab_store.record_history(u, t);
                    }
                    let form_json = form_data.and_then(|v| serde_json::to_string(&v).ok());
                    let _ = self
                        .tab_store
                        .update_tab_state(id, scroll_x, scroll_y, title, url, form_json);
                    if had_title || had_url {
                        self.sync_tabs_to_topbar();
                    }
                }
            }
            ClientIpcMessage::Navigate { url } => {
                let target_tab_id = tab_id.or_else(|| self.tab_store.active_tab_id());
                if let Some(id) = target_tab_id {
                    let resolved = self.search_engine.resolve_query_or_url(&url);
                    let cleaned = self.url_cleaner.clean_url(&resolved);
                    if self.traffic_filter.should_block(&cleaned, "browser://local") {
                        warn!("Blocked navigation to: {}", cleaned);
                        return;
                    }
                    if let Some(tab) = self.tab_store.get_tab_mut(id) {
                        if let Some(wv) = tab.state.as_active_mut() {
                            let _ = wv.load_url(&cleaned);
                        }
                    }
                }
            }
            ClientIpcMessage::OmnibarQuery { query } => {
                let history_items = self.tab_store.query_history(&query, 6).unwrap_or_default();
                let history_matches: Vec<HistoryMatch> = history_items
                    .into_iter()
                    .map(|h| HistoryMatch {
                        title: h.title,
                        url: h.url,
                        bookmarked: h.bookmarked,
                    })
                    .collect();
                let suggestions = self.search_engine.build_suggestions(&query, &history_matches);
                if let Ok(json_str) = serde_json::to_string(&suggestions) {
                    if let Some(tab) = self.tab_store.get_active_tab() {
                        if let Some(wv) = tab.state.as_active() {
                            let script = format!(
                                "if (window.__FeatherEngine && window.__FeatherEngine.updateOmnibarSuggestions) {{ window.__FeatherEngine.updateOmnibarSuggestions({}); }}",
                                json_str
                            );
                            let _ = wv.evaluate_script(&script);
                        }
                    }
                }
            }
            ClientIpcMessage::NewTab { url, parent_id } => {
                let target_url = url.unwrap_or_else(|| "https://example.com".to_string());
                let _ = self.create_tab(window, &target_url, parent_id);
            }
            ClientIpcMessage::SwitchTab { id } => {
                let _ = self.switch_to_tab(window, id);
            }
            ClientIpcMessage::SwitchTabIndex { index } => {
                let _ = self.switch_tab_by_index(window, index);
            }
            ClientIpcMessage::CloseTab { id } => {
                let _ = self.close_tab(id);
                if self.tab_store.total_tab_count() == 0 {
                    let _ = self.create_tab(window, "https://example.com", None);
                }
            }
            ClientIpcMessage::CloseCurrentTab => {
                let _ = self.close_current_tab(window);
            }
            ClientIpcMessage::NextTab => {
                let _ = self.cycle_next_tab(window);
            }
            ClientIpcMessage::PrevTab => {
                let _ = self.cycle_prev_tab(window);
            }
            ClientIpcMessage::OpenOmnibar => {
                if let Some(tab) = self.tab_store.get_active_tab() {
                    if let Some(wv) = tab.state.as_active() {
                        let _ = wv.evaluate_script("if (window.__FeatherEngine && window.__FeatherEngine.showOmnibar) { window.__FeatherEngine.showOmnibar(); }");
                    }
                }
            }
            ClientIpcMessage::ToggleDarkMode => {
                if let Some(tab) = self.tab_store.get_active_tab() {
                    if let Some(wv) = tab.state.as_active() {
                        let _ = wv.evaluate_script("if (window.__FeatherEngine) { window.__FeatherEngine.toggleDarkMode(); }");
                    }
                }
            }
            ClientIpcMessage::ToggleVimKeys => {
                if let Some(tab) = self.tab_store.get_active_tab() {
                    if let Some(wv) = tab.state.as_active() {
                        let _ = wv.evaluate_script(
                            "if (window.__FeatherEngine) { window.__FeatherEngine.toggleVimKeys(); }",
                        );
                    }
                }
            }
            ClientIpcMessage::SitePrefsRequest => {
                if let Some(id) = tab_id {
                    self.send_site_prefs(id);
                }
            }
            ClientIpcMessage::SetSitePref { key, value } => {
                if let Some(tab) = tab_id.and_then(|id| self.tab_store.get_tab(id)) {
                    let origin = origin_of(&tab.url);
                    if let Err(e) = self.tab_store.set_site_pref(&origin, key, value) {
                        warn!("Could not save {:?} for {}: {:?}", key, origin, e);
                    } else {
                        info!("{:?} = {} for {}", key, value, origin);
                    }
                }
            }
            ClientIpcMessage::HistoryBack => {
                self.eval_in_active("history.back();");
            }
            ClientIpcMessage::HistoryForward => {
                self.eval_in_active("history.forward();");
            }
            ClientIpcMessage::Zoom { step } => {
                self.apply_zoom(step);
            }
            ClientIpcMessage::ToggleFullscreen => {
                self.toggle_fullscreen(window);
            }
            ClientIpcMessage::OpenFind => {
                self.eval_in_active(
                    "if (window.__FeatherEngine) { window.__FeatherEngine.showFind(); }",
                );
            }
            ClientIpcMessage::MediaState { playing, muted } => {
                let target = tab_id.or_else(|| self.tab_store.active_tab_id());
                if let Some(id) = target {
                    let started = playing && !self.tab_store.is_playing(id);
                    if self.tab_store.set_media_state(id, playing, muted) {
                        self.sync_tabs_to_topbar();
                    }
                    // Offer the saved position once, when playback starts — not on every
                    // state change, or a pause would drag the viewer backwards.
                    if started {
                        let at = self
                            .tab_store
                            .get_tab(id)
                            .map(|t| self.tab_store.media_position(&t.url))
                            .unwrap_or(0);
                        // Evaluated in the tab that reported, not the active one: a
                        // background tab can start playing and must not seek the foreground.
                        if at > 60 {
                            if let Some(wv) =
                                self.tab_store.get_tab(id).and_then(|t| t.state.as_active())
                            {
                                let _ = wv.evaluate_script(&format!(
                                    "if (window.__FeatherVideo) {{ window.__FeatherVideo.resumeAt({}); }}",
                                    at
                                ));
                            }
                        }
                    }
                }
            }
            ClientIpcMessage::MediaProgress { position, duration } => {
                let target = tab_id.or_else(|| self.tab_store.active_tab_id());
                if let Some(tab) = target.and_then(|id| self.tab_store.get_tab(id)) {
                    let url = tab.url.clone();
                    if let Err(e) = self.tab_store.record_media_position(&url, position, duration) {
                        warn!("Could not record playback position: {:?}", e);
                    }
                }
            }
            ClientIpcMessage::ToggleTabMuted { id } => {
                let now_muted = !self
                    .tab_store
                    .get_tab(id)
                    .map(|t| t.muted)
                    .unwrap_or(false);
                if let Some(wv) = self
                    .tab_store
                    .get_tab(id)
                    .and_then(|t| t.state.as_active())
                {
                    let _ = wv.evaluate_script(&format!(
                        "if (window.__FeatherVideo) {{ window.__FeatherVideo.setMuted({}); }}",
                        now_muted
                    ));
                }
                let playing = self.tab_store.is_playing(id);
                if self.tab_store.set_media_state(id, playing, now_muted) {
                    self.sync_tabs_to_topbar();
                }
            }
            ClientIpcMessage::DownloadMedia { url, referer } => {
                let name = suggested_filename(&url);
                if let Some(dest) = crate::platform::ask_save_path(&name) {
                    let tx = self.event_sender.clone();
                    let _ = tx.send(AppEvent::Download {
                        name: name.clone(),
                        state: DownloadState::Started,
                    });
                    // Its own thread: this one is the event loop, and the transfer is
                    // minutes long.
                    std::thread::spawn(move || {
                        let segmented = url.contains(".m3u8") || url.contains(".mpd");
                        let run = if segmented {
                            crate::network::hls::download
                        } else {
                            crate::network::accel::download
                        };
                        run(
                            &url,
                            &dest,
                            referer.as_deref(),
                            &name,
                            &|u| {
                                use crate::network::accel::State;
                                match u.state {
                                    State::Running => {
                                        let _ = tx.send(AppEvent::DownloadProgress {
                                            name: u.name,
                                            done: u.done,
                                            total: u.total,
                                            bps: u.bps,
                                        });
                                    }
                                    State::Done => {
                                        let _ = tx.send(AppEvent::Download {
                                            name: u.name,
                                            state: DownloadState::Finished,
                                        });
                                    }
                                    State::Failed(ref why) => {
                                        warn!("Accelerated download failed: {}", why);
                                        let _ = tx.send(AppEvent::Download {
                                            name: u.name,
                                            state: DownloadState::Failed,
                                        });
                                    }
                                }
                            },
                        );
                    });
                }
            }
            ClientIpcMessage::ToggleStreamPanel => {
                self.eval_in_active(
                    "if (window.__FeatherVideo) { window.__FeatherVideo.stream.toggle(); }",
                );
            }
            ClientIpcMessage::ToggleVideoPanel => {
                self.eval_in_active(
                    "if (window.__FeatherEngine) { window.__FeatherEngine.toggleVideoPanel(); }",
                );
            }
            ClientIpcMessage::ToggleBookmark => {
                let target = tab_id.or_else(|| self.tab_store.active_tab_id());
                if let Some(tab) = target.and_then(|id| self.tab_store.get_tab(id)) {
                    let (url, title) = (tab.url.clone(), tab.title.clone());
                    match self.tab_store.toggle_bookmark(&url, &title) {
                        Ok(now_on) => {
                            info!("{} {}", if now_on { "Bookmarked" } else { "Unbookmarked" }, url);
                            self.sync_bookmark_to_topbar();
                        }
                        Err(e) => warn!("Could not bookmark {}: {:?}", url, e),
                    }
                }
            }
            ClientIpcMessage::ToggleBionic => {
                if let Some(tab) = self.tab_store.get_active_tab() {
                    if let Some(wv) = tab.state.as_active() {
                        let _ = wv.evaluate_script("if (window.__FeatherEngine) { window.__FeatherEngine.toggleBionicReading(); }");
                    }
                }
            }
        }
    }

    /// Puts the window in and out of fullscreen.
    ///
    /// Borderless rather than exclusive: exclusive fullscreen takes a video mode and
    /// changes the display's resolution, which is for games, not for reading a page.
    /// The resize event that follows re-lays out the topbar and the live view already.
    pub fn toggle_fullscreen(&self, window: &Window) {
        let now = if window.fullscreen().is_some() {
            None
        } else {
            Some(winit::window::Fullscreen::Borderless(None))
        };
        window.set_fullscreen(now);
    }

    /// Runs a script in whichever tab is in front, if it has a live view.
    fn eval_in_active(&self, script: &str) {
        if let Some(tab) = self.tab_store.get_active_tab() {
            if let Some(wv) = tab.state.as_active() {
                let _ = wv.evaluate_script(script);
            }
        }
    }

    /// Steps the front tab's zoom. `step` is -1 out, +1 in, 0 back to 100%.
    fn apply_zoom(&mut self, step: i32) {
        let Some(id) = self.tab_store.active_tab_id() else {
            return;
        };
        let current = *self.zoom_levels.get(&id).unwrap_or(&1.0);
        let level = match step {
            0 => 1.0,
            n => (current * 1.1f64.powi(n)).clamp(0.4, 3.0),
        };
        self.zoom_levels.insert(id, level);
        if let Some(tab) = self.tab_store.get_tab(id) {
            if let Some(wv) = tab.state.as_active() {
                let _ = wv.zoom(level);
            }
        }
        if let Some(topbar) = &self.topbar_webview {
            let _ = topbar.evaluate_script(&format!(
                "if (window.__updateZoom) {{ window.__updateZoom({}); }}",
                (level * 100.0).round()
            ));
        }
    }

    /// Lights the star when the page in front is bookmarked.
    pub fn sync_bookmark_to_topbar(&self) {
        let starred = self
            .tab_store
            .get_active_tab()
            .map(|t| self.tab_store.is_bookmarked(&t.url))
            .unwrap_or(false);
        if let Some(topbar) = &self.topbar_webview {
            let _ = topbar.evaluate_script(&format!(
                "if (window.__updateBookmark) {{ window.__updateBookmark({}); }}",
                starred
            ));
        }
    }

    fn build_webview(
        &self,
        window: &Window,
        tab_id: u64,
        url: &str,
        restore_from: Option<&TabSnapshot>,
    ) -> wry::Result<WebView> {
        let ctx = self.webview_context();
        Self::construct_webview(&ctx, window, tab_id, url, restore_from)
    }

    fn construct_webview(
        ctx: &WebViewContext,
        window: &Window,
        tab_id: u64,
        url: &str,
        restore_from: Option<&TabSnapshot>,
    ) -> wry::Result<WebView> {
        let logical = ctx.size.to_logical::<f64>(ctx.scale_factor);
        let content_height = (logical.height - TOPBAR_HEIGHT).max(0.0);
        let bounds = Rect {
            position: LogicalPosition::new(0.0, TOPBAR_HEIGHT).into(),
            size: LogicalSize::new(logical.width, content_height).into(),
        };

        let mut builder = WebViewBuilder::new()
            .with_bounds(bounds)
            .with_initialization_script(RUNTIME_SCRIPT)
            .with_initialization_script_for_main_only(VIDEO_SCRIPT, false);

        // Put scroll position and form contents back if this tab is returning from dormancy.
        let restore_script = restore_from.map(generate_restore_script);
        if let Some(script) = restore_script.as_deref() {
            builder = builder.with_initialization_script(script);
        }

        // Pre-DOM navigation interceptor.
        let filter_clone = Rc::clone(&ctx.filter);
        let cleaner_clone = ctx.cleaner.clone();
        let redirect_tx = ctx.tx.clone();
        builder = builder.with_navigation_handler(move |nav_url| {
            if filter_clone.should_block(&nav_url, "browser://local") {
                warn!("[Pre-DOM] Blocked navigation request: {}", nav_url);
                return false;
            }

            let cleaned = cleaner_clone.clean_url(&nav_url);
            if cleaned != nav_url {
                info!("[Pre-DOM] Stripped tracking parameters: {} -> {}", nav_url, cleaned);
                let _ = redirect_tx.send(AppEvent::Redirect {
                    tab_id,
                    url: cleaned,
                });
                // Deny this load; the host re-issues the clean one. `clean_url` is
                // idempotent, so the replacement passes this check and does not loop.
                return false;
            }

            true
        });

        // Downloads. WebKit hands us the request and a path to fill in; the native save
        // panel runs modally on this same thread, so the answer is ready when we return.
        let start_tx = ctx.tx.clone();
        builder = builder.with_download_started_handler(move |url, path| {
            let name = suggested_filename(&url);
            match crate::platform::ask_save_path(&name) {
                Some(chosen) => {
                    info!("Downloading {} to {}", url, chosen.display());
                    *path = chosen.clone();
                    let _ = start_tx.send(AppEvent::Download {
                        name: name.clone(),
                        state: DownloadState::Started,
                    });

                    // WebKit owns the transfer, so progress is read off the file it is
                    // filling. The size has to be asked for separately, which is a network
                    // call and does not belong on the thread WebKit is waiting on.
                    let watch_tx = start_tx.clone();
                    let watch_url = url.clone();
                    std::thread::spawn(move || {
                        let total = crate::network::accel::probe(&watch_url, None)
                            .map(|(n, _)| n)
                            .unwrap_or(0);
                        crate::network::accel::watch(chosen, total, name, &|u| {
                            let _ = watch_tx.send(AppEvent::DownloadProgress {
                                name: u.name,
                                done: u.done,
                                total: u.total,
                                bps: u.bps,
                            });
                        });
                    });
                    true
                }
                None => {
                    info!("Download of {} cancelled at the save panel.", url);
                    false
                }
            }
        });

        let done_tx = ctx.tx.clone();
        builder = builder.with_download_completed_handler(move |url, _path, success| {
            let _ = done_tx.send(AppEvent::Download {
                name: suggested_filename(&url),
                state: if success {
                    DownloadState::Finished
                } else {
                    DownloadState::Failed
                },
            });
        });

        // IPC handler linking client runtime events to the Rust host
        let ipc_tx = ctx.tx.clone();
        builder = builder.with_ipc_handler(move |req| {
            let body = req.body().to_string();
            let _ = ipc_tx.send(AppEvent::TabIpc {
                tab_id,
                raw_json: body,
            });
        });

        let webview = builder.with_url(url).build_as_child(window)?;
        ctx.blocker.apply_to(&webview);
        Ok(webview)
    }
}
