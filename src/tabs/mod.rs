pub mod state;

pub use state::{
    generate_restore_script, origin_of, HistoryItem, SitePref, SitePrefs, Tab, TabError, TabSnapshot,
    TabState, TabStore, TabSummary,
    max_active_views, MAX_ACTIVE_VIEWS_CEILING, MIN_ACTIVE_VIEWS,
};
