pub mod app;
pub mod paths;
pub mod profile;
pub mod network;
pub mod platform;
pub mod tabs;

use app::BrowserCore;
use log::{error, info};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

pub struct FeatherApp {
    window: Option<Window>,
    browser: Option<BrowserCore>,
    initial_url: Option<String>,
    bench: Option<profile::Bench>,
    modifiers: ModifiersState,
}

impl FeatherApp {
    pub fn new(initial_url: Option<String>, bench: Option<profile::Bench>) -> Self {
        Self {
            window: None,
            browser: None,
            initial_url,
            bench,
            modifiers: ModifiersState::default(),
        }
    }
}

impl ApplicationHandler for FeatherApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window_attributes = WindowAttributes::default()
                .with_title("FeatherBrowser Engine")
                .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0));

            let window = match event_loop.create_window(window_attributes) {
                Ok(w) => w,
                Err(e) => {
                    error!("Failed to create winit window: {:?}", e);
                    event_loop.exit();
                    return;
                }
            };

            let mut browser = match BrowserCore::new() {
                Ok(b) => b,
                Err(e) => {
                    error!("Failed to initialize BrowserCore: {:?}", e);
                    event_loop.exit();
                    return;
                }
            };

            let inner_size = window.inner_size();
            let scale_factor = window.scale_factor();
            browser.handle_resize(inner_size, scale_factor);

            if let Err(e) = browser.init_topbar(&window) {
                error!("Failed to initialize topbar chrome: {:?}", e);
            }

            // Bench mode starts from nothing: a restored session would make the tab counts
            // in the table mean something different on every run.
            let restored = if self.bench.is_some() {
                0
            } else {
                match browser.restore_session(&window) {
                    Ok(n) => n,
                    Err(e) => {
                        error!("Could not restore the last session: {:?}", e);
                        0
                    }
                }
            };

            // An explicit URL opens in addition to whatever came back, so `feather <url>`
            // never costs you the session.
            let open_url = self.initial_url.as_deref().or({
                if restored == 0 && self.bench.is_none() {
                    Some("https://example.com")
                } else {
                    None
                }
            });

            if let Some(url) = open_url {
                if let Err(e) = browser.create_tab(&window, url, None) {
                    error!("Failed to create initial tab: {:?}", e);
                }
            }

            self.browser = Some(browser);
            self.window = Some(window);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                info!("FeatherBrowser window close requested. Exiting.");
                event_loop.exit();
            }
            WindowEvent::Resized(physical_size) => {
                if let (Some(browser), Some(window)) = (&mut self.browser, &self.window) {
                    browser.handle_resize(physical_size, window.scale_factor());
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let (Some(browser), Some(window)) = (&mut self.browser, &self.window) {
                    browser.handle_resize(window.inner_size(), scale_factor);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        ref logical_key,
                        ..
                    },
                ..
            } => {
                if let (Some(browser), Some(window)) = (&mut self.browser, &self.window) {
                    let has_super = self.modifiers.super_key();
                    let has_ctrl = self.modifiers.control_key();
                    let has_alt = self.modifiers.alt_key();
                    let has_shift = self.modifiers.shift_key();

                    if has_super {
                        match logical_key {
                            Key::Character(c) => match c.as_str() {
                                "t" | "T" => {
                                    let _ = browser.create_tab(window, "https://example.com", None);
                                }
                                "w" | "W" => {
                                    let _ = browser.close_current_tab(window);
                                }
                                "1" => { let _ = browser.switch_tab_by_index(window, 0); }
                                "2" => { let _ = browser.switch_tab_by_index(window, 1); }
                                "3" => { let _ = browser.switch_tab_by_index(window, 2); }
                                "4" => { let _ = browser.switch_tab_by_index(window, 3); }
                                "5" => { let _ = browser.switch_tab_by_index(window, 4); }
                                "6" => { let _ = browser.switch_tab_by_index(window, 5); }
                                "7" => { let _ = browser.switch_tab_by_index(window, 6); }
                                "8" => { let _ = browser.switch_tab_by_index(window, 7); }
                                "9" => { let _ = browser.switch_tab_by_index(window, 8); }
                                "l" | "L" => {
                                    if let Some(tab) = browser.tab_store.get_active_tab() {
                                        if let Some(wv) = tab.state.as_active() {
                                            let _ = wv.evaluate_script("if (window.__FeatherEngine) { window.__FeatherEngine.showOmnibar(); }");
                                        }
                                    }
                                }
                                _ => {}
                            },
                            Key::Named(NamedKey::ArrowRight) if has_alt => {
                                let _ = browser.cycle_next_tab(window);
                            }
                            Key::Named(NamedKey::ArrowLeft) if has_alt => {
                                let _ = browser.cycle_prev_tab(window);
                            }
                            _ => {}
                        }
                    } else if has_ctrl {
                        if let Key::Named(NamedKey::Tab) = logical_key {
                            if has_shift {
                                let _ = browser.cycle_prev_tab(window);
                            } else {
                                let _ = browser.cycle_next_tab(window);
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let (Some(browser), Some(window)) = (&mut self.browser, &self.window) {
                    browser.process_pending_events(window);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let (Some(browser), Some(window)) = (&mut self.browser, &self.window) {
            browser.process_pending_events(window);

            if let Some(bench) = &mut self.bench {
                let live = browser.tab_store.active_tab_count();
                let total = browser.tab_store.total_tab_count();
                match bench.tick(live, total) {
                    profile::BenchStep::Wait => {}
                    profile::BenchStep::Open(url) => {
                        if let Err(e) = browser.create_tab(window, url, None) {
                            error!("Bench could not open {}: {:?}", url, e);
                        }
                    }
                    profile::BenchStep::Finish => {
                        bench.report();
                        event_loop.exit();
                        return;
                    }
                }
                // Nothing else wakes the loop while pages load, so ask to be woken.
                event_loop.set_control_flow(ControlFlow::WaitUntil(bench.next_at()));
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("Starting FeatherBrowser Engine v0.1.0");

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // --profile logs a memory reading on every tab event. --bench[=N] opens N pages from a
    // fixed list, measures each step and prints a table.
    let args: Vec<String> = std::env::args().skip(1).collect();
    profile::set_enabled(args.iter().any(|a| a == "--profile"));

    let bench = args.iter().find(|a| a.starts_with("--bench")).map(|flag| {
        let count = flag
            .split_once('=')
            .and_then(|(_, n)| n.parse::<usize>().ok())
            .unwrap_or(5);
        info!("Bench mode: opening {} pages, measuring each.", count);
        profile::Bench::new(count, 6)
    });

    // Support URL or search query passed as command-line argument (e.g. cargo run -- "rust wry" or https://crates.io)
    // With no argument, the last session is restored instead of opening a default page.
    let search_engine = network::SearchEngine::default();
    let initial_url = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(|raw| search_engine.resolve_query_or_url(raw));

    match &initial_url {
        Some(url) => info!("Initial URL configured: {}", url),
        None => info!("No URL given; restoring the last session."),
    }
    let mut app = FeatherApp::new(initial_url, bench);
    event_loop.run_app(&mut app)?;

    Ok(())
}
