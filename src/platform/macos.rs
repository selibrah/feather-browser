//! Attaches WebKit content rules to wry's WKWebViews.
//!
//! WebKit compiles a rule list asynchronously and calls back on the main queue, so this
//! cannot be a function that returns a ready list. Instead compilation starts at launch,
//! the result lands in a shared slot, and `AppEvent::ContentRulesReady` tells the event
//! loop to attach it to the WebViews that already exist.

use block2::RcBlock;
use log::{error, info, warn};
use objc2::rc::Retained;
use objc2_foundation::{NSError, NSString};
use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use wry::{WebView, WebViewExtMacOS};

use crate::app::AppEvent;
use crate::network::{ContentRules, RuleChunk};

/// Holds the compiled rule list once WebKit hands it over.
///
/// Everything here runs on the main thread: `start` is called during setup, and WebKit
/// delivers both completion handlers to the main queue. That is what makes the `Rc`
/// interior sound despite crossing an Objective-C block boundary.
#[derive(Clone, Default)]
pub struct ContentBlocker {
    lists: Rc<RefCell<Vec<Retained<WKContentRuleList>>>>,
    rule_count: Rc<RefCell<usize>>,
}

impl ContentBlocker {
    pub fn new() -> Self {
        Self::default()
    }

    /// True once WebKit has handed back at least one usable rule list.
    pub fn is_ready(&self) -> bool {
        !self.lists.borrow().is_empty()
    }

    /// Number of rules across every active list, or 0 if none is active yet.
    pub fn rule_count(&self) -> usize {
        *self.rule_count.borrow()
    }

    /// Attaches the rule list to a WebView. Safe to call before the list is ready — the
    /// event loop re-attaches to every live tab when compilation finishes.
    pub fn apply_to(&self, webview: &WebView) {
        let lists = self.lists.borrow();
        if lists.is_empty() {
            return;
        }
        // Safety: `manager()` is wry's own WKUserContentController for this WebView, and
        // addContentRuleList: takes a borrowed list it retains itself. Clearing first keeps
        // this idempotent — chunks finish compiling one at a time, so a WebView can be
        // re-applied several times as more lists arrive.
        unsafe {
            let manager = webview.manager();
            manager.removeAllContentRuleLists();
            for list in lists.iter() {
                manager.addContentRuleList(list);
            }
        }
    }

    /// Attaches rules a previous launch already compiled, without touching a filter list.
    ///
    /// This is the common path. Only the first launch after a list changes pays to convert
    /// EasyList; every launch after that costs one store lookup per chunk. If WebKit has
    /// evicted a list, `rebuild` supplies the rules the slow way and the manifest is dropped.
    pub fn start_from_manifest(
        &self,
        chunks: Vec<(String, usize)>,
        notify: Sender<AppEvent>,
        rebuild: Rc<dyn Fn() -> Option<ContentRules>>,
    ) {
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore() }) else {
            warn!("No default WKContentRuleListStore available; subresource blocking is off.");
            return;
        };

        // One missing list is enough to rebuild, and the rebuild must happen only once even
        // though every lookup can miss.
        let recovering = Rc::new(std::cell::Cell::new(false));

        for (identifier, chunk_rules) in chunks {
            let slot = Rc::clone(&self.lists);
            let counter = Rc::clone(&self.rule_count);
            let notify = notify.clone();
            let blocker = self.clone();
            let rebuild = Rc::clone(&rebuild);
            let recovering = Rc::clone(&recovering);
            let ns_identifier = NSString::from_str(&identifier);

            let on_lookup = RcBlock::new(move |list: *mut WKContentRuleList, _err: *mut NSError| {
                if install(&slot, &counter, list, chunk_rules, &notify, "loaded from WebKit's store") {
                    return;
                }

                if recovering.replace(true) {
                    return;
                }

                warn!("WebKit no longer has rule list {}; rebuilding from the filter lists.", identifier);
                crate::network::rules::forget_manifest(&crate::paths::data_dir());
                if let Some(rules) = rebuild() {
                    blocker.start(rules, notify.clone());
                }
            });

            unsafe {
                store.lookUpContentRuleListForIdentifier_completionHandler(
                    Some(&ns_identifier),
                    Some(&on_lookup),
                )
            };
        }
    }

    /// Begins compilation of every chunk and returns immediately.
    ///
    /// Looks each list up in WebKit's on-disk store first. The identifier is derived from
    /// the rule content, so an unchanged filter list is compiled once ever, not once per
    /// launch — which matters, because compiling a full EasyList takes seconds.
    pub fn start(&self, rules: ContentRules, notify: Sender<AppEvent>) {
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore() }) else {
            warn!("No default WKContentRuleListStore available; subresource blocking is off.");
            return;
        };

        for chunk in rules.chunks {
            self.start_chunk(&store, chunk, notify.clone());
        }
    }

    /// Compiles one rule list. Chunks land independently; each arrival re-applies the whole
    /// set to every live tab, so blocking strengthens as they come in rather than waiting.
    fn start_chunk(
        &self,
        store: &Retained<WKContentRuleListStore>,
        chunk: RuleChunk,
        notify: Sender<AppEvent>,
    ) {
        let identifier = NSString::from_str(&chunk.identifier);
        let json = NSString::from_str(&chunk.json);
        let rule_count = chunk.rule_count;

        let slot = Rc::clone(&self.lists);
        let counter = Rc::clone(&self.rule_count);
        let compile_store = store.clone();
        let compile_id = identifier.clone();

        let on_lookup = RcBlock::new(move |list: *mut WKContentRuleList, _err: *mut NSError| {
            if install(&slot, &counter, list, rule_count, &notify, "loaded from WebKit's store") {
                return;
            }

            // Not cached. Compiling a full list is slow, hence the log line.
            info!("Compiling {} content rules; blocking starts once WebKit finishes.", rule_count);

            let slot = Rc::clone(&slot);
            let counter = Rc::clone(&counter);
            let notify = notify.clone();
            let json = json.clone();

            let on_compile = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
                if install(&slot, &counter, list, rule_count, &notify, "compiled") {
                    return;
                }
                let reason = unsafe { err.as_ref() }
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "no error reported".to_string());
                error!(
                    "WebKit refused a content rule list ({}). Those {} rules are not \
                     blocking; try lowering network::rules::RULES_PER_CHUNK.",
                    reason, rule_count
                );
            });

            unsafe {
                compile_store
                    .compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                        Some(&compile_id),
                        Some(&json),
                        Some(&on_compile),
                    )
            };
        });

        unsafe {
            store.lookUpContentRuleListForIdentifier_completionHandler(
                Some(&identifier),
                Some(&on_lookup),
            )
        };
    }
}

/// Stores a rule list WebKit handed back. Returns false when the pointer is null, which is
/// how both a cache miss and a compile failure arrive.
fn install(
    slot: &Rc<RefCell<Vec<Retained<WKContentRuleList>>>>,
    counter: &Rc<RefCell<usize>>,
    list: *mut WKContentRuleList,
    rule_count: usize,
    notify: &Sender<AppEvent>,
    source: &str,
) -> bool {
    // Safety: WebKit hands back an autoreleased list; retain it to outlive the callback.
    let Some(list) = (unsafe { Retained::retain(list) }) else {
        return false;
    };

    slot.borrow_mut().push(list);
    *counter.borrow_mut() += rule_count;
    info!(
        "Content rule list {} ({} rules). Subresource blocking is active.",
        source, rule_count
    );
    let _ = notify.send(AppEvent::ContentRulesReady);
    true
}

/// Asks the user where to put a download, seeded with the filename the server suggested.
///
/// Runs the panel modally on the main thread, which is where WebKit calls the download
/// handler from. Returning `None` means the user cancelled, which the caller turns into a
/// refused download.
pub fn ask_save_path(suggested_name: &str) -> Option<std::path::PathBuf> {
    use objc2_app_kit::{NSModalResponseOK, NSSavePanel};
    use objc2_foundation::MainThreadMarker;

    let mtm = MainThreadMarker::new()?;
    unsafe {
        let panel = NSSavePanel::savePanel(mtm);
        panel.setNameFieldStringValue(&NSString::from_str(suggested_name));
        panel.setCanCreateDirectories(true);
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        let url = panel.URL()?;
        let path = std::path::PathBuf::from(url.path()?.to_string());
        // WKDownload refuses a destination that already exists. The panel has already
        // asked about replacing, so honour that answer here rather than failing the
        // download with "cancelled" and no explanation.
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                warn!("Could not replace {}: {}", path.display(), e);
                return None;
            }
        }
        Some(path)
    }
}
