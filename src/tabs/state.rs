use log::{info, warn};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Fewest live WebViews the browser will ever allow itself.
///
/// Below two, switching between a page and the one you just came from would suspend and
/// rehydrate on every single switch.
pub const MIN_ACTIVE_VIEWS: usize = 2;

/// Most live WebViews, however much RAM the machine has. Past this the LRU stops being a
/// memory policy and the browser is just an ordinary tab hog with extra steps.
pub const MAX_ACTIVE_VIEWS_CEILING: usize = 8;

/// One live WebView per this much physical RAM.
///
/// A loaded WebContent process ran 30–50 MB in measurement, but the figure that matters is
/// what a heavy page costs, and that is far larger and far more variable. This is deliberately
/// conservative: the whole point of the project is to leave the machine alone.
const RAM_PER_VIEW: u64 = 4 * 1024 * 1024 * 1024;

/// How many live WebViews this machine gets.
///
/// `FEATHER_MAX_ACTIVE_VIEWS` overrides it — the escape hatch for a machine that disagrees
/// with the arithmetic.
// ponytail: derived once at startup from total RAM, not from what is free right now.
// Sampling pressure at runtime is a different feature; this one only has to stop a 64 GB
// machine being held to the same two views as an 8 GB one.
pub fn max_active_views() -> usize {
    if let Some(override_value) = std::env::var("FEATHER_MAX_ACTIVE_VIEWS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return override_value.max(1);
    }

    crate::profile::total_ram()
        .map(|ram| (ram / RAM_PER_VIEW) as usize)
        .unwrap_or(MIN_ACTIVE_VIEWS)
        .clamp(MIN_ACTIVE_VIEWS, MAX_ACTIVE_VIEWS_CEILING)
}

#[derive(Error, Debug)]
pub enum TabError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Tab not found: {0}")]
    NotFound(u64),
    #[error("Tab is already active: {0}")]
    AlreadyActive(u64),
    #[error("Tab is already dormant: {0}")]
    AlreadyDormant(u64),
    #[error("Rehydration failed: {0}")]
    RehydrationFailed(String),
}

/// Persistent snapshot of a suspended tab stored in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabSnapshot {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub scroll_x: i32,
    pub scroll_y: i32,
    pub parent_id: Option<u64>,
    pub last_accessed: u64,
    pub form_data: Option<String>,
}

/// Summary representation of a tab for the topbar chrome UI and tab strip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabSummary {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub is_active: bool,
    pub is_dormant: bool,
    pub parent_id: Option<u64>,
    /// Whether this tab is playing media right now. Not persisted: a dormant tab has no
    /// WebView, so by definition it is not playing anything.
    pub playing: bool,
    pub muted: bool,
}

/// Persisted navigation history entry in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryItem {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    pub last_visited: u64,
    /// Bookmarks are history rows with a flag, not a second table: a bookmark is a page
    /// you visited, and the omnibar already ranks this table.
    pub bookmarked: bool,
}

/// Lifecycle state of a tab in FeatherBrowser.
pub enum TabState<W = wry::WebView> {
    /// Live WebView running in system RAM.
    Active(W),
    /// OS WebView dropped (0 MB RAM). Metadata preserved in snapshot.
    Dormant(TabSnapshot),
}

impl<W> TabState<W> {
    pub fn is_active(&self) -> bool {
        matches!(self, TabState::Active(_))
    }

    pub fn is_dormant(&self) -> bool {
        matches!(self, TabState::Dormant(_))
    }

    pub fn as_active(&self) -> Option<&W> {
        match self {
            TabState::Active(w) => Some(w),
            TabState::Dormant(_) => None,
        }
    }

    pub fn as_active_mut(&mut self) -> Option<&mut W> {
        match self {
            TabState::Active(w) => Some(w),
            TabState::Dormant(_) => None,
        }
    }

    pub fn snapshot(&self) -> Option<&TabSnapshot> {
        match self {
            TabState::Dormant(s) => Some(s),
            TabState::Active(_) => None,
        }
    }
}

/// Per-site settings. `None` means the user never said, so the default applies.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct SitePrefs {
    pub dark_mode: Option<bool>,
    pub vim_keys: Option<bool>,
}

/// Which per-site setting is being written. An enum rather than a string because the name
/// is interpolated into SQL — this is what makes that safe.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SitePref {
    DarkMode,
    VimKeys,
}

impl SitePref {
    fn column_name(self) -> &'static str {
        match self {
            SitePref::DarkMode => "dark_mode",
            SitePref::VimKeys => "vim_keys",
        }
    }
}

/// The origin a URL belongs to, for keying site preferences. Falls back to the whole string
/// when it will not parse, so a preference is at worst scoped oddly, never lost.
pub fn origin_of(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|_| url.to_string())
}

/// Metadata and state for a single browser tab.
pub struct Tab<W = wry::WebView> {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub scroll_x: i32,
    pub scroll_y: i32,
    pub parent_id: Option<u64>,
    pub last_accessed: u64,
    pub form_data: Option<String>,
    pub state: TabState<W>,
    pub playing: bool,
    pub muted: bool,
}

impl<W> Tab<W> {
    pub fn to_snapshot(&self) -> TabSnapshot {
        TabSnapshot {
            id: self.id,
            title: self.title.clone(),
            url: self.url.clone(),
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
            parent_id: self.parent_id,
            last_accessed: self.last_accessed,
            form_data: self.form_data.clone(),
        }
    }
}

/// LRU Tab Store managing tab lifecycles, active counts, and SQLite persistence.
pub struct TabStore<W = wry::WebView> {
    tabs: HashMap<u64, Tab<W>>,
    active_tab_id: Option<u64>,
    next_id: u64,
    max_active_views: usize,
    db: Connection,
}

impl<W> TabStore<W> {
    /// Opens a TabStore with SQLite persistence at the given database path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, TabError> {
        let conn = Connection::open(path)?;
        Self::init_db(&conn)?;
        let next_id = Self::next_id_after_existing(&conn)?;
        Ok(Self {
            tabs: HashMap::new(),
            active_tab_id: None,
            next_id,
            max_active_views: max_active_views(),
            db: conn,
        })
    }

    /// Ids continue where the last session stopped.
    ///
    /// Restarting the counter at 1 meant a new session's tab 1 upserted over the previous
    /// session's tab 1 — the rows were there, and then they were not.
    // ponytail: max(id) + 1 rather than a persisted counter. The snapshot table is the
    // session, so it already holds the high-water mark; closing every tab is allowed to
    // reset the numbering.
    fn next_id_after_existing(conn: &Connection) -> Result<u64, rusqlite::Error> {
        conn.query_row(
            "SELECT COALESCE(MAX(id), 0) + 1 FROM tab_snapshots",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n as u64)
    }

    /// Creates an in-memory TabStore (ideal for tests and ephemeral sessions).
    pub fn in_memory() -> Result<Self, TabError> {
        let conn = Connection::open_in_memory()?;
        Self::init_db(&conn)?;
        Ok(Self {
            tabs: HashMap::new(),
            active_tab_id: None,
            next_id: 1,
            max_active_views: max_active_views(),
            db: conn,
        })
    }

    /// The live-WebView ceiling this store is enforcing.
    pub fn max_active_views(&self) -> usize {
        self.max_active_views
    }

    /// Overrides the ceiling. Tests pin it so they do not depend on the host's RAM.
    pub fn set_max_active_views(&mut self, max: usize) {
        self.max_active_views = max.max(1);
    }

    /// Opens `sessions.db` in the platform data directory.
    pub fn new_default() -> Result<Self, TabError> {
        Self::open(crate::paths::ensure_data_dir().join("sessions.db"))
    }

    fn init_db(conn: &Connection) -> Result<(), rusqlite::Error> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tab_snapshots (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                url TEXT NOT NULL,
                scroll_x INTEGER NOT NULL,
                scroll_y INTEGER NOT NULL,
                parent_id INTEGER,
                last_accessed INTEGER NOT NULL,
                form_data TEXT
            );",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS navigation_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                visit_count INTEGER NOT NULL DEFAULT 1,
                last_visited INTEGER NOT NULL,
                bookmarked INTEGER NOT NULL DEFAULT 0,
                media_position INTEGER NOT NULL DEFAULT 0
            );",
            [],
        )?;

        // Preferences the user set for one site, and nothing else. A row exists only for a
        // site they actually toggled something on.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS site_prefs (
                origin TEXT PRIMARY KEY,
                dark_mode INTEGER,
                vim_keys INTEGER
            );",
            [],
        )?;

        // SQLite has no ADD COLUMN IF NOT EXISTS. On a database that already has the
        // column this fails, which is the success case; on an older one it adds it.
        let _ = conn.execute(
            "ALTER TABLE navigation_history ADD COLUMN bookmarked INTEGER NOT NULL DEFAULT 0;",
            [],
        );
        // Where playback had got to, on the same table for the same reason bookmarks are:
        // a page you were watching is a page you visited.
        let _ = conn.execute(
            "ALTER TABLE navigation_history ADD COLUMN media_position INTEGER NOT NULL DEFAULT 0;",
            [],
        );

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_url ON navigation_history(url);",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_title ON navigation_history(title);",
            [],
        )?;

        Ok(())
    }

    pub fn active_tab_id(&self) -> Option<u64> {
        self.active_tab_id
    }

    /// The id the next `insert_active_tab` will assign.
    ///
    /// A WebView has to be built before the tab exists, and it stamps every IPC message
    /// it sends with its tab id, so the caller needs to know the id in advance. Deriving
    /// it from the tab count instead is wrong the moment a tab is closed.
    pub fn peek_next_id(&self) -> u64 {
        self.next_id
    }

    pub fn total_tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_tab_count(&self) -> usize {
        self.tabs.values().filter(|t| t.state.is_active()).count()
    }

    pub fn dormant_tab_count(&self) -> usize {
        self.tabs.values().filter(|t| t.state.is_dormant()).count()
    }

    pub fn get_tab(&self, id: u64) -> Option<&Tab<W>> {
        self.tabs.get(&id)
    }

    pub fn get_tab_mut(&mut self, id: u64) -> Option<&mut Tab<W>> {
        self.tabs.get_mut(&id)
    }

    /// Every live WebView, for work that has to reach all of them at once.
    pub fn active_webviews(&self) -> impl Iterator<Item = &W> {
        self.tabs.values().filter_map(|t| t.state.as_active())
    }

    pub fn get_active_tab(&self) -> Option<&Tab<W>> {
        self.active_tab_id.and_then(|id| self.tabs.get(&id))
    }

    pub fn get_active_tab_mut(&mut self) -> Option<&mut Tab<W>> {
        self.active_tab_id.and_then(|id| self.tabs.get_mut(&id))
    }

    /// Returns an ordered list of tab summaries for the UI tab strip.
    pub fn get_tab_summaries(&self) -> Vec<TabSummary> {
        let mut tabs: Vec<&Tab<W>> = self.tabs.values().collect();
        tabs.sort_by_key(|t| t.id);
        tabs.into_iter()
            .map(|t| TabSummary {
                id: t.id,
                title: t.title.clone(),
                url: t.url.clone(),
                is_active: self.active_tab_id == Some(t.id),
                is_dormant: t.state.is_dormant(),
                parent_id: t.parent_id,
                playing: t.playing,
                muted: t.muted,
            })
            .collect()
    }

    /// Gets a tab ID by its 0-indexed position in creation order (for Cmd+1..9).
    pub fn get_tab_id_by_index(&self, index: usize) -> Option<u64> {
        let mut ids: Vec<u64> = self.tabs.keys().copied().collect();
        ids.sort();
        ids.get(index).copied()
    }

    /// Gets the next tab ID in circular order.
    pub fn next_tab_id(&self) -> Option<u64> {
        let mut ids: Vec<u64> = self.tabs.keys().copied().collect();
        if ids.is_empty() {
            return None;
        }
        ids.sort();
        let current_id = self.active_tab_id.unwrap_or(ids[0]);
        if let Some(pos) = ids.iter().position(|&id| id == current_id) {
            let next_pos = (pos + 1) % ids.len();
            Some(ids[next_pos])
        } else {
            Some(ids[0])
        }
    }

    /// Gets the previous tab ID in circular order.
    pub fn prev_tab_id(&self) -> Option<u64> {
        let mut ids: Vec<u64> = self.tabs.keys().copied().collect();
        if ids.is_empty() {
            return None;
        }
        ids.sort();
        let current_id = self.active_tab_id.unwrap_or(ids[0]);
        if let Some(pos) = ids.iter().position(|&id| id == current_id) {
            let prev_pos = if pos == 0 { ids.len() - 1 } else { pos - 1 };
            Some(ids[prev_pos])
        } else {
            Some(ids[0])
        }
    }

    /// Inserts a new active tab, enforcing the live-WebView ceiling by
    /// hibernating the oldest inactive tab if threshold is reached.
    pub fn insert_active_tab(
        &mut self,
        title: String,
        url: String,
        parent_id: Option<u64>,
        webview: W,
    ) -> Result<u64, TabError> {
        let id = self.next_id;
        self.next_id += 1;
        let now = now_millis();

        let tab = Tab {
            id,
            title,
            url,
            scroll_x: 0,
            scroll_y: 0,
            parent_id,
            last_accessed: now,
            form_data: None,
            state: TabState::Active(webview),
            playing: false,
            muted: false,
        };

        // The snapshot table is the session, not just the dormant set: a tab that is still
        // active when the browser quits has to come back too.
        self.save_snapshot_to_db(&tab.to_snapshot())?;

        self.tabs.insert(id, tab);
        self.active_tab_id = Some(id);

        // Enforce the live-WebView ceiling
        self.enforce_lru_ceiling(Some(id))?;

        Ok(id)
    }

    /// Focuses an existing tab by ID. If dormant, caller should call `rehydrate_tab`.
    pub fn focus_tab(&mut self, id: u64) -> Result<(), TabError> {
        if !self.tabs.contains_key(&id) {
            return Err(TabError::NotFound(id));
        }

        let now = now_millis();
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.last_accessed = now;
        }

        self.active_tab_id = Some(id);
        self.enforce_lru_ceiling(Some(id))?;
        Ok(())
    }

    /// Updates the scroll coordinates and title of a tab (called via IPC handler).
    pub fn update_tab_state(
        &mut self,
        id: u64,
        scroll_x: i32,
        scroll_y: i32,
        title: Option<String>,
        url: Option<String>,
        form_data: Option<String>,
    ) -> Result<(), TabError> {
        let tab = self.tabs.get_mut(&id).ok_or(TabError::NotFound(id))?;
        tab.scroll_x = scroll_x;
        tab.scroll_y = scroll_y;
        if let Some(t) = title {
            if !t.is_empty() {
                tab.title = t;
            }
        }
        if let Some(u) = url {
            if !u.is_empty() {
                // Leaving the page drops what was typed on it. Otherwise those values get
                // replayed into any later page that happens to use the same field names.
                if u != tab.url {
                    tab.form_data = None;
                }
                tab.url = u;
            }
        }
        // A report with no fields leaves the previous contents alone: the page sends state
        // on scroll too, and an empty form should not erase what is already saved.
        if form_data.is_some() {
            tab.form_data = form_data;
        }
        tab.last_accessed = now_millis();

        // ponytail: writes on every scroll report. The page throttles those to a few per
        // second and this is a local upsert on one indexed row; batch it if a profile ever
        // says otherwise.
        let snapshot = tab.to_snapshot();
        self.save_snapshot_to_db(&snapshot)?;
        Ok(())
    }

    /// Explicitly suspends an active tab, persisting its snapshot to SQLite and dropping its WebView handle.
    pub fn suspend_tab(&mut self, id: u64) -> Result<TabSnapshot, TabError> {
        let snapshot = {
            let tab = self.tabs.get(&id).ok_or(TabError::NotFound(id))?;
            if tab.state.is_dormant() {
                return Err(TabError::AlreadyDormant(id));
            }
            tab.to_snapshot()
        };

        // Write snapshot to SQLite sessions.db
        self.save_snapshot_to_db(&snapshot)?;

        // Replace state with Dormant, causing OS WebView to be explicitly dropped
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.state = TabState::Dormant(snapshot.clone());
        }

        info!(
            "Hibernated tab #{}: Dropped WebView handle, persisted to SQLite. URL: {}",
            id, snapshot.url
        );

        Ok(snapshot)
    }

    /// Rehydrates a dormant tab using a provided constructor closure that instantiates the new WebView.
    pub fn rehydrate_tab<F>(&mut self, id: u64, factory: F) -> Result<(), TabError>
    where
        F: FnOnce(&TabSnapshot) -> Result<W, TabError>,
    {
        let tab = self.tabs.get(&id).ok_or(TabError::NotFound(id))?;
        let snapshot = match &tab.state {
            TabState::Dormant(s) => s.clone(),
            TabState::Active(_) => return Err(TabError::AlreadyActive(id)),
        };

        // Before adding a new active view, ensure we have room under the ceiling
        self.enforce_lru_ceiling_before_activation(id)?;

        // Construct the new WebView handle
        let new_webview = factory(&snapshot)?;

        let tab_mut = self.tabs.get_mut(&id).ok_or(TabError::NotFound(id))?;
        tab_mut.last_accessed = now_millis();
        tab_mut.state = TabState::Active(new_webview);
        self.active_tab_id = Some(id);

        info!(
            "Rehydrated tab #{}: Spawened fresh WebView. URL: {}, Scroll: ({}, {})",
            id, snapshot.url, snapshot.scroll_x, snapshot.scroll_y
        );

        Ok(())
    }

    /// Closes and removes a tab, dropping any active WebView and cleaning SQLite records.
    pub fn close_tab(&mut self, id: u64) -> Result<(), TabError> {
        let removed = self.tabs.remove(&id).ok_or(TabError::NotFound(id))?;
        drop(removed);

        // Delete from database
        self.db.execute("DELETE FROM tab_snapshots WHERE id = ?1", params![id as i64])?;

        if self.active_tab_id == Some(id) {
            // Pick most recently accessed tab as new active tab
            self.active_tab_id = self
                .tabs
                .values()
                .max_by_key(|t| (t.last_accessed, t.id))
                .map(|t| t.id);
        }

        info!("Closed tab #{}", id);
        Ok(())
    }

    /// Enforces `active_count <= max_active_views`. If exceeded, suspends least recently used inactive tab.
    fn enforce_lru_ceiling(&mut self, current_protected_id: Option<u64>) -> Result<(), TabError> {
        while self.active_tab_count() > self.max_active_views {
            let oldest_active_id = self.eviction_candidate(current_protected_id, None);

            match oldest_active_id {
                Some(evict_id) => {
                    self.suspend_tab(evict_id)?;
                }
                None => {
                    warn!("Could not find suitable candidate to suspend, breaking ceiling loop");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Least recently used tab that may be hibernated.
    ///
    /// A tab playing media is never the answer while anything else will do. Least-recently-
    /// *accessed* is exactly what a movie playing in a background tab is, so a plain LRU
    /// picks it first and stops the film to save memory nobody asked to save. Playing tabs
    /// stay eligible as a last resort, or the ceiling would stop being a ceiling.
    fn eviction_candidate(&self, protected: Option<u64>, upcoming: Option<u64>) -> Option<u64> {
        let eligible = |t: &&Tab<W>| {
            t.state.is_active()
                && protected != Some(t.id)
                && upcoming != Some(t.id)
        };
        self.tabs
            .values()
            .filter(eligible)
            .filter(|t| !t.playing)
            .min_by_key(|t| (t.last_accessed, t.id))
            .or_else(|| self.tabs.values().filter(eligible).min_by_key(|t| (t.last_accessed, t.id)))
            .map(|t| t.id)
    }

    /// Marks whether a tab is currently playing media. Returns whether anything changed.
    pub fn set_media_state(&mut self, id: u64, playing: bool, muted: bool) -> bool {
        match self.tabs.get_mut(&id) {
            Some(tab) if tab.playing != playing || tab.muted != muted => {
                tab.playing = playing;
                tab.muted = muted;
                true
            }
            _ => false,
        }
    }

    pub fn is_playing(&self, id: u64) -> bool {
        self.tabs.get(&id).map(|t| t.playing).unwrap_or(false)
    }

    /// Pre-check before rehydrating a tab so active count does not exceed the ceiling - 1.
    fn enforce_lru_ceiling_before_activation(&mut self, upcoming_id: u64) -> Result<(), TabError> {
        while self.active_tab_count() >= self.max_active_views {
            let oldest_active_id = self.eviction_candidate(None, Some(upcoming_id));

            match oldest_active_id {
                Some(evict_id) => {
                    self.suspend_tab(evict_id)?;
                }
                None => {
                    break;
                }
            }
        }
        Ok(())
    }

    fn save_snapshot_to_db(&self, snapshot: &TabSnapshot) -> Result<(), TabError> {
        self.db.execute(
            "INSERT INTO tab_snapshots (id, title, url, scroll_x, scroll_y, parent_id, last_accessed, form_data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                url = excluded.url,
                scroll_x = excluded.scroll_x,
                scroll_y = excluded.scroll_y,
                parent_id = excluded.parent_id,
                last_accessed = excluded.last_accessed,
                form_data = excluded.form_data;",
            params![
                snapshot.id as i64,
                snapshot.title,
                snapshot.url,
                snapshot.scroll_x,
                snapshot.scroll_y,
                snapshot.parent_id.map(|p| p as i64),
                snapshot.last_accessed as i64,
                snapshot.form_data,
            ],
        )?;
        Ok(())
    }

    /// What the user has turned on for this site. `None` for a setting they never touched.
    pub fn site_prefs(&self, origin: &str) -> SitePrefs {
        self.db
            .query_row(
                "SELECT dark_mode, vim_keys FROM site_prefs WHERE origin = ?1",
                params![origin],
                |row| {
                    Ok(SitePrefs {
                        dark_mode: row.get::<_, Option<i64>>(0)?.map(|v| v != 0),
                        vim_keys: row.get::<_, Option<i64>>(1)?.map(|v| v != 0),
                    })
                },
            )
            .unwrap_or_default()
    }

    /// Records one preference for one site. `column` is a fixed name, never user input.
    pub fn set_site_pref(&self, origin: &str, column: SitePref, value: bool) -> Result<(), TabError> {
        let name = column.column_name();
        self.db.execute(
            &format!(
                "INSERT INTO site_prefs (origin, {name}) VALUES (?1, ?2)
                 ON CONFLICT(origin) DO UPDATE SET {name} = excluded.{name}"
            ),
            params![origin, value as i64],
        )?;
        Ok(())
    }

    /// Rebuilds last session's tabs as dormant entries, newest first.
    ///
    /// Nothing is rendered here: every restored tab is `Dormant` and costs a row in a
    /// HashMap until it is focused, which is the whole point of the suspension design.
    /// Returns the id of the tab that was active when the session ended.
    pub fn restore_session(&mut self) -> Result<Option<u64>, TabError> {
        let snapshots = self.load_snapshots_from_db()?;
        if snapshots.is_empty() {
            return Ok(None);
        }

        // `load_snapshots_from_db` orders newest-touched first, so the first row is the tab
        // that was in front.
        // ponytail: last_accessed as a stand-in for "was focused". Persist active_tab_id if
        // that ever proves wrong.
        let most_recent = snapshots[0].id;

        for snapshot in snapshots {
            let id = snapshot.id;
            self.tabs.insert(
                id,
                Tab {
                    id,
                    title: snapshot.title.clone(),
                    url: snapshot.url.clone(),
                    scroll_x: snapshot.scroll_x,
                    scroll_y: snapshot.scroll_y,
                    parent_id: snapshot.parent_id,
                    last_accessed: snapshot.last_accessed,
                    form_data: snapshot.form_data.clone(),
                    state: TabState::Dormant(snapshot),
                    playing: false,
                    muted: false,
                },
            );
            self.next_id = self.next_id.max(id + 1);
        }

        info!("Restored {} tab(s) from the last session.", self.tabs.len());
        Ok(Some(most_recent))
    }

    /// Loads all persisted snapshots from SQLite.
    pub fn load_snapshots_from_db(&self) -> Result<Vec<TabSnapshot>, TabError> {
        let mut stmt = self.db.prepare(
            // The id tie-breaks: timestamps are in milliseconds and two tabs opened in the
            // same one would otherwise come back in whatever order SQLite felt like.
            "SELECT id, title, url, scroll_x, scroll_y, parent_id, last_accessed, form_data \
             FROM tab_snapshots ORDER BY last_accessed DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let parent_id_val: Option<i64> = row.get(5)?;
            Ok(TabSnapshot {
                id: row.get::<_, i64>(0)? as u64,
                title: row.get(1)?,
                url: row.get(2)?,
                scroll_x: row.get(3)?,
                scroll_y: row.get(4)?,
                parent_id: parent_id_val.map(|p| p as u64),
                last_accessed: row.get::<_, i64>(6)? as u64,
                form_data: row.get(7)?,
            })
        })?;

        let mut snapshots = Vec::new();
        for s in rows {
            snapshots.push(s?);
        }
        Ok(snapshots)
    }

    /// Upserts a visited URL into navigation history, incrementing visit count and updating timestamp.
    pub fn record_history(&self, url: &str, title: &str) -> Result<(), TabError> {
        let trimmed_url = url.trim();
        if trimmed_url.is_empty()
            || trimmed_url == "about:blank"
            || trimmed_url.starts_with("data:")
        {
            return Ok(());
        }

        let now = now_millis();
        self.db.execute(
            "INSERT INTO navigation_history (url, title, visit_count, last_visited)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(url) DO UPDATE SET
                title = CASE WHEN excluded.title != '' THEN excluded.title ELSE navigation_history.title END,
                visit_count = navigation_history.visit_count + 1,
                last_visited = excluded.last_visited;",
            params![trimmed_url, title.trim(), now as i64],
        )?;

        Ok(())
    }

    /// Queries navigation history for prefix or substring matches in URL or title.
    pub fn query_history(&self, query: &str, limit: usize) -> Result<Vec<HistoryItem>, TabError> {
        let trimmed = query.trim();
        let pattern = format!("%{}%", trimmed);

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<HistoryItem> {
            Ok(HistoryItem {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                visit_count: row.get(3)?,
                last_visited: row.get::<_, i64>(4)? as u64,
                bookmarked: row.get::<_, i64>(5)? != 0,
            })
        };

        let mut items = Vec::new();
        if trimmed.is_empty() {
            let mut stmt = self.db.prepare(
                "SELECT id, url, title, visit_count, last_visited, bookmarked
                 FROM navigation_history
                 ORDER BY bookmarked DESC, last_visited DESC
                 LIMIT ?1;",
            )?;
            let rows = stmt.query_map(params![limit as i64], map_row)?;
            for item in rows {
                items.push(item?);
            }
        } else {
            let mut stmt = self.db.prepare(
                "SELECT id, url, title, visit_count, last_visited, bookmarked
                 FROM navigation_history
                 WHERE url LIKE ?1 OR title LIKE ?1
                 ORDER BY bookmarked DESC, visit_count DESC, last_visited DESC
                 LIMIT ?2;",
            )?;
            let rows = stmt.query_map(params![pattern, limit as i64], map_row)?;
            for item in rows {
                items.push(item?);
            }
        }

        Ok(items)
    }

    /// Flips the bookmark flag on a URL, returning what it is now.
    ///
    /// The row is created if the page was never recorded — bookmarking a page you are
    /// looking at should not depend on the state report having landed first.
    pub fn toggle_bookmark(&self, url: &str, title: &str) -> Result<bool, TabError> {
        let url = url.trim();
        if url.is_empty() {
            return Ok(false);
        }
        let now = now_millis();
        self.db.execute(
            "INSERT INTO navigation_history (url, title, visit_count, last_visited, bookmarked)
             VALUES (?1, ?2, 1, ?3, 1)
             ON CONFLICT(url) DO UPDATE SET bookmarked = 1 - navigation_history.bookmarked;",
            params![url, title.trim(), now as i64],
        )?;
        Ok(self.is_bookmarked(url))
    }

    /// Remembers how far into a page's video the viewer got.
    ///
    /// Stored only between one and two minutes from either end: below that there is nothing
    /// to resume, and near the end the viewer has finished and wants the next thing.
    pub fn record_media_position(&self, url: &str, position: i64, duration: i64) -> Result<(), TabError> {
        let keep = position > 60 && (duration <= 0 || position < duration - 60);
        self.db.execute(
            "UPDATE navigation_history SET media_position = ?1 WHERE url = ?2",
            rusqlite::params![if keep { position } else { 0 }, url],
        )?;
        Ok(())
    }

    pub fn media_position(&self, url: &str) -> i64 {
        self.db
            .query_row(
                "SELECT media_position FROM navigation_history WHERE url = ?1",
                [url],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.db
            .query_row(
                "SELECT bookmarked FROM navigation_history WHERE url = ?1;",
                params![url.trim()],
                |row| row.get::<_, i64>(0),
            )
            .map(|v| v != 0)
            .unwrap_or(false)
    }
}

/// Generates the JavaScript that puts a rehydrated tab back where it was: scroll position
/// and whatever the page last reported of its form fields.
pub fn generate_restore_script(snapshot: &TabSnapshot) -> String {
    // serde_json emits a valid JS string literal, which is what keeps arbitrary field
    // contents from escaping into the surrounding source.
    let form_literal = snapshot
        .form_data
        .as_deref()
        .and_then(|d| serde_json::to_string(d).ok())
        .unwrap_or_else(|| "null".to_string());

    format!(
        r#"(function() {{
            var formJson = {form};
            function restore() {{
                if (window.__FeatherEngine && typeof window.__FeatherEngine.restoreScroll === 'function') {{
                    window.__FeatherEngine.restoreScroll({x}, {y});
                }} else {{
                    window.scrollTo({x}, {y});
                }}
                if (formJson && window.__FeatherEngine && typeof window.__FeatherEngine.restoreForm === 'function') {{
                    try {{ window.__FeatherEngine.restoreForm(JSON.parse(formJson)); }} catch (e) {{}}
                }}
            }}
            if (document.readyState === 'complete' || document.readyState === 'interactive') {{
                setTimeout(restore, 50);
            }} else {{
                window.addEventListener('DOMContentLoaded', restore);
            }}
        }})();"#,
        form = form_literal,
        x = snapshot.scroll_x,
        y = snapshot.scroll_y
    )
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct MockWebView {
        id: u64,
        is_alive: bool,
    }

    impl MockWebView {
        fn new(id: u64) -> Self {
            Self { id, is_alive: true }
        }
    }

    impl Drop for MockWebView {
        fn drop(&mut self) {
            self.is_alive = false;
        }
    }

    #[test]
    fn test_tab_suspension_lru_invariance() {
        let mut store: TabStore<MockWebView> = TabStore::in_memory().expect("In-memory store init");
        // Pinned, so the invariant under test does not depend on how much RAM the test
        // machine happens to have.
        store.set_max_active_views(2);

        // 1. Create Tab 1
        let id1 = store
            .insert_active_tab(
                "Tab 1".into(),
                "https://example.com/1".into(),
                None,
                MockWebView::new(1),
            )
            .expect("Create Tab 1");
        assert_eq!(store.active_tab_count(), 1);
        assert_eq!(store.dormant_tab_count(), 0);

        // Update scroll position for Tab 1
        store.update_tab_state(id1, 0, 450, None, None, None).unwrap();

        // 2. Create Tab 2
        let id2 = store
            .insert_active_tab(
                "Tab 2".into(),
                "https://example.com/2".into(),
                None,
                MockWebView::new(2),
            )
            .expect("Create Tab 2");
        assert_eq!(store.active_tab_count(), 2);
        assert_eq!(store.dormant_tab_count(), 0);

        // 3. Create Tab 3: Exceeds the ceiling of 2.
        // Tab 1 is the oldest inactive tab, so it MUST be suspended into TabState::Dormant.
        let id3 = store
            .insert_active_tab(
                "Tab 3".into(),
                "https://example.com/3".into(),
                None,
                MockWebView::new(3),
            )
            .expect("Create Tab 3");

        // Verify: Exactly 2 active WebViews remain in TabStore
        assert_eq!(store.active_tab_count(), 2, "Active count must be strictly capped at the ceiling");
        assert_eq!(store.dormant_tab_count(), 1, "Tab 1 must have transitioned to Dormant");

        // Verify: Tab 1 converted to TabState::Dormant
        let tab1 = store.get_tab(id1).expect("Tab 1 exists");
        assert!(tab1.state.is_dormant(), "Tab 1 must be dormant");
        if let TabState::Dormant(snapshot) = &tab1.state {
            assert_eq!(snapshot.url, "https://example.com/1");
            assert_eq!(snapshot.scroll_y, 450);
        } else {
            panic!("Tab 1 state must be Dormant");
        }

        // Verify Tab 2 and Tab 3 are still active
        assert!(store.get_tab(id2).unwrap().state.is_active());
        assert!(store.get_tab(id3).unwrap().state.is_active());

        // Every open tab is persisted, not just the hibernated one — the table is the
        // session. Tab 1's row carries the scroll position it was suspended at.
        let snapshots = store.load_snapshots_from_db().unwrap();
        assert_eq!(snapshots.len(), 3);
        let tab1_row = snapshots.iter().find(|s| s.id == id1).expect("tab 1 persisted");
        assert_eq!(tab1_row.scroll_y, 450);

        // 4. Rehydrate Tab 1:
        // Tab 2 was accessed before Tab 3, so Tab 2 should now be suspended!
        store
            .rehydrate_tab(id1, |s| Ok(MockWebView::new(s.id)))
            .expect("Rehydrate Tab 1");

        assert_eq!(store.active_tab_count(), 2);
        assert_eq!(store.dormant_tab_count(), 1);

        assert!(store.get_tab(id1).unwrap().state.is_active());
        assert!(store.get_tab(id3).unwrap().state.is_active());
        assert!(store.get_tab(id2).unwrap().state.is_dormant(), "Tab 2 should have been evicted to make room for Tab 1");
    }

    #[test]
    fn test_close_tab_cleans_db() {
        let mut store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        store.set_max_active_views(2);
        let id1 = store
            .insert_active_tab("T1".into(), "https://1.com".into(), None, MockWebView::new(1))
            .unwrap();
        let _id2 = store
            .insert_active_tab("T2".into(), "https://2.com".into(), None, MockWebView::new(2))
            .unwrap();
        let _id3 = store
            .insert_active_tab("T3".into(), "https://3.com".into(), None, MockWebView::new(3))
            .unwrap();

        // All three are persisted; closing one removes exactly its row.
        let in_db = store.load_snapshots_from_db().unwrap();
        assert_eq!(in_db.len(), 3);

        store.close_tab(id1).unwrap();
        assert_eq!(store.total_tab_count(), 2);
        let in_db_after = store.load_snapshots_from_db().unwrap();
        assert_eq!(in_db_after.len(), 2);
        assert!(!in_db_after.iter().any(|s| s.id == id1), "closed tab must leave no row");
    }

    #[test]
    fn test_site_prefs_are_per_origin_and_default_to_unset() {
        let store: TabStore<MockWebView> = TabStore::in_memory().unwrap();

        // Nothing is on until the user says so — vim keys capturing bare keystrokes on a
        // site that never asked for it is the bug this replaces.
        let untouched = store.site_prefs("https://github.com");
        assert_eq!(untouched.vim_keys, None);
        assert_eq!(untouched.dark_mode, None);

        store.set_site_pref("https://github.com", SitePref::VimKeys, true).unwrap();
        store.set_site_pref("https://github.com", SitePref::DarkMode, true).unwrap();
        store.set_site_pref("https://news.ycombinator.com", SitePref::DarkMode, false).unwrap();

        let gh = store.site_prefs("https://github.com");
        assert_eq!(gh.vim_keys, Some(true));
        assert_eq!(gh.dark_mode, Some(true));

        // A second setting on one origin must not disturb the first, and must not leak.
        let hn = store.site_prefs("https://news.ycombinator.com");
        assert_eq!(hn.dark_mode, Some(false));
        assert_eq!(hn.vim_keys, None, "a preference must not leak across origins");

        store.set_site_pref("https://github.com", SitePref::VimKeys, false).unwrap();
        let gh = store.site_prefs("https://github.com");
        assert_eq!(gh.vim_keys, Some(false));
        assert_eq!(gh.dark_mode, Some(true), "updating one column must not clear the other");
    }

    #[test]
    fn test_origin_of_ignores_path_and_query() {
        assert_eq!(origin_of("https://github.com/user/repo?x=1#f"), "https://github.com");
        assert_eq!(origin_of("https://a.github.com/"), "https://a.github.com");
        assert_ne!(
            origin_of("https://github.com/"),
            origin_of("http://github.com/"),
            "scheme is part of the origin"
        );
        // Unparseable input still keys something rather than collapsing every site together.
        assert_eq!(origin_of("not a url"), "not a url");
    }

    #[test]
    fn test_ceiling_adapts_and_stays_in_bounds() {
        // The point of the phase: a 64 GB machine should not be held to the same two live
        // views as an 8 GB one, and no machine gets an unbounded number.
        let derived = max_active_views();
        assert!(
            (MIN_ACTIVE_VIEWS..=MAX_ACTIVE_VIEWS_CEILING).contains(&derived),
            "derived ceiling {} outside [{}, {}]",
            derived, MIN_ACTIVE_VIEWS, MAX_ACTIVE_VIEWS_CEILING
        );

        // The store must actually enforce whatever it was given, not the constant.
        let mut store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        store.set_max_active_views(3);
        for i in 1..=5 {
            store
                .insert_active_tab(format!("T{}", i), format!("https://{}.com", i), None, MockWebView::new(i))
                .unwrap();
        }
        assert_eq!(store.active_tab_count(), 3, "a ceiling of 3 must hold 3 live views");
        assert_eq!(store.dormant_tab_count(), 2);
    }

    #[test]
    fn test_a_playing_tab_survives_the_ceiling() {
        // The bug this is here for: hibernation picks the least recently *accessed* tab,
        // which is precisely what a movie playing in a background tab looks like.
        let mut store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        store.set_max_active_views(3);
        for i in 1..=3 {
            store
                .insert_active_tab(format!("T{}", i), format!("https://{}.com", i), None, MockWebView::new(i))
                .unwrap();
        }
        // Tab 1 is the oldest and would be evicted first, but it is playing.
        assert!(store.set_media_state(1, true, false));
        store
            .insert_active_tab("T4".into(), "https://4.com".into(), None, MockWebView::new(4))
            .unwrap();

        assert_eq!(store.active_tab_count(), 3, "the ceiling still holds");
        assert!(store.is_playing(1));
        assert!(
            store.get_tab(1).map(|t| t.state.is_active()).unwrap_or(false),
            "the playing tab must not be the one hibernated"
        );
        assert!(
            store.get_tab(2).map(|t| t.state.is_dormant()).unwrap_or(false),
            "the next-oldest silent tab goes instead"
        );

        // Last resort: when every live tab is playing, the ceiling still wins.
        store.set_media_state(3, true, false);
        store.set_media_state(4, true, false);
        store
            .insert_active_tab("T5".into(), "https://5.com".into(), None, MockWebView::new(5))
            .unwrap();
        assert_eq!(
            store.active_tab_count(),
            3,
            "a ceiling that playing tabs can lift is not a ceiling"
        );
    }

    #[test]
    fn test_bookmark_toggles_and_outranks_plain_history() {
        let store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        store.record_history("https://a.example/", "A").unwrap();
        store.record_history("https://b.example/", "B").unwrap();
        // b is visited far more often, so only the star can put a ahead of it.
        for _ in 0..5 {
            store.record_history("https://b.example/", "B").unwrap();
        }

        assert!(!store.is_bookmarked("https://a.example/"));
        assert!(store.toggle_bookmark("https://a.example/", "A").unwrap());
        assert!(store.is_bookmarked("https://a.example/"));

        let hits = store.query_history("example", 10).unwrap();
        assert_eq!(hits[0].url, "https://a.example/", "a bookmark ranks above history");
        assert!(hits[0].bookmarked);

        assert!(!store.toggle_bookmark("https://a.example/", "A").unwrap());
        assert!(!store.is_bookmarked("https://a.example/"));
    }

    #[test]
    fn test_bookmarking_an_unvisited_page_creates_the_row() {
        let store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        assert!(store.toggle_bookmark("https://never.example/", "Never").unwrap());
        let hits = store.query_history("never", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].bookmarked);
    }

    #[test]
    fn test_navigation_drops_the_previous_page_form_data() {
        let mut store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        store.set_max_active_views(2);
        let id = store
            .insert_active_tab("T".into(), "https://forum.example/post".into(), None, MockWebView::new(1))
            .unwrap();

        store
            .update_tab_state(id, 0, 0, None, None, Some(r#"{"message":"private draft"}"#.into()))
            .unwrap();
        assert!(store.get_tab(id).unwrap().form_data.is_some());

        // Same page, a plain scroll report: the draft stays.
        store.update_tab_state(id, 0, 300, None, Some("https://forum.example/post".into()), None).unwrap();
        assert!(store.get_tab(id).unwrap().form_data.is_some(), "scrolling must not erase a draft");

        // Different page: the draft must not follow it there.
        store.update_tab_state(id, 0, 0, None, Some("https://unrelated.example/".into()), None).unwrap();
        assert_eq!(
            store.get_tab(id).unwrap().form_data, None,
            "form values must not be carried across a navigation"
        );
    }

    #[test]
    fn test_session_survives_a_restart() {
        let path = std::env::temp_dir().join(format!("feather-session-{}.db", now_millis()));
        let _ = std::fs::remove_file(&path);

        let (id1, id2) = {
            let mut store: TabStore<MockWebView> = TabStore::open(&path).unwrap();
            store.set_max_active_views(2);
            let id1 = store
                .insert_active_tab("One".into(), "https://1.com".into(), None, MockWebView::new(1))
                .unwrap();
            let id2 = store
                .insert_active_tab("Two".into(), "https://2.com".into(), Some(id1), MockWebView::new(2))
                .unwrap();
            store
                .update_tab_state(id2, 0, 900, Some("Two".into()), None, Some(r#"{"q":"draft"}"#.into()))
                .unwrap();
            (id1, id2)
        };

        // Second launch: same database, no tabs in memory.
        let mut restored: TabStore<MockWebView> = TabStore::open(&path).unwrap();
        restored.set_max_active_views(2);
        let front = restored.restore_session().unwrap();

        assert_eq!(front, Some(id2), "the last-touched tab comes back in front");
        assert_eq!(restored.total_tab_count(), 2);
        assert_eq!(restored.active_tab_count(), 0, "restored tabs cost no WebView");
        assert_eq!(restored.get_tab(id2).unwrap().scroll_y, 900, "scroll position survives");
        assert_eq!(restored.get_tab(id2).unwrap().parent_id, Some(id1), "tree survives");
        assert_eq!(
            restored.get_tab(id2).unwrap().form_data.as_deref(),
            Some(r#"{"q":"draft"}"#),
            "what was typed survives"
        );

        // The id counter must not hand out an id that is already on disk.
        let fresh = restored
            .insert_active_tab("Three".into(), "https://3.com".into(), None, MockWebView::new(3))
            .unwrap();
        assert!(fresh > id2, "new ids continue past the restored session, got {}", fresh);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_tab_summaries_and_navigation() {
        let mut store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        store.set_max_active_views(2);
        let id1 = store
            .insert_active_tab("Tab 1".into(), "https://1.com".into(), None, MockWebView::new(1))
            .unwrap();
        let id2 = store
            .insert_active_tab("Tab 2".into(), "https://2.com".into(), None, MockWebView::new(2))
            .unwrap();
        let id3 = store
            .insert_active_tab("Tab 3".into(), "https://3.com".into(), None, MockWebView::new(3))
            .unwrap();

        let summaries = store.get_tab_summaries();
        assert_eq!(summaries.len(), 3);
        assert_eq!(summaries[0].id, id1);
        assert_eq!(summaries[0].title, "Tab 1");
        assert!(summaries[0].is_dormant); // evicted because the ceiling is pinned to 2
        assert!(!summaries[0].is_active);

        assert_eq!(summaries[1].id, id2);
        assert!(!summaries[1].is_dormant);
        assert!(!summaries[1].is_active);

        assert_eq!(summaries[2].id, id3);
        assert!(!summaries[2].is_dormant);
        assert!(summaries[2].is_active);

        // Test index lookup
        assert_eq!(store.get_tab_id_by_index(0), Some(id1));
        assert_eq!(store.get_tab_id_by_index(1), Some(id2));
        assert_eq!(store.get_tab_id_by_index(2), Some(id3));
        assert_eq!(store.get_tab_id_by_index(3), None);

        // Test next / prev navigation
        // Current active is id3
        assert_eq!(store.next_tab_id(), Some(id1)); // loops to first
        assert_eq!(store.prev_tab_id(), Some(id2));

        // Switch active tab to id2
        store.active_tab_id = Some(id2);
        assert_eq!(store.next_tab_id(), Some(id3));
        assert_eq!(store.prev_tab_id(), Some(id1));

        // Switch active tab to id1
        store.active_tab_id = Some(id1);
        assert_eq!(store.next_tab_id(), Some(id2));
        assert_eq!(store.prev_tab_id(), Some(id3)); // loops to last
    }

    #[test]
    fn test_peek_next_id_survives_a_close() {
        let mut store: TabStore<MockWebView> = TabStore::in_memory().unwrap();
        store.set_max_active_views(2);
        for n in 1..=3 {
            let expected = store.peek_next_id();
            let id = store
                .insert_active_tab(format!("T{}", n), format!("https://{}.com", n), None, MockWebView::new(n))
                .unwrap();
            assert_eq!(id, expected, "peek must match the id actually assigned");
        }

        store.close_tab(2).unwrap();

        // Two tabs remain, so the old `total_tab_count() + 1` guess would say 3 — an id
        // already in use. The next tab's IPC would have landed on tab 3's state.
        assert_eq!(store.total_tab_count(), 2);
        let expected = store.peek_next_id();
        assert_eq!(expected, 4);
        let id = store
            .insert_active_tab("T4".into(), "https://4.com".into(), None, MockWebView::new(4))
            .unwrap();
        assert_eq!(id, expected);
    }

    #[test]
    fn test_record_and_query_history() {
        let store: TabStore<MockWebView> = TabStore::in_memory().unwrap();

        store.record_history("https://news.ycombinator.com", "Hacker News").unwrap();
        store.record_history("https://www.rust-lang.org", "Rust Programming Language").unwrap();
        store.record_history("https://news.ycombinator.com", "Hacker News").unwrap(); // 2nd visit

        let all_history = store.query_history("", 10).unwrap();
        assert_eq!(all_history.len(), 2);

        // Ranked by visit count (HN has 2 visits)
        let hn_query = store.query_history("hacker", 5).unwrap();
        assert_eq!(hn_query.len(), 1);
        assert_eq!(hn_query[0].url, "https://news.ycombinator.com");
        assert_eq!(hn_query[0].visit_count, 2);

        let rust_query = store.query_history("rust", 5).unwrap();
        assert_eq!(rust_query.len(), 1);
        assert_eq!(rust_query[0].url, "https://www.rust-lang.org");
    }
}
