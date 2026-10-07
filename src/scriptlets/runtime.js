/**
 * FeatherBrowser Unified Client Ingestion Runtime
 * Zero-dependency client scriptlet combining:
 * 1. Vim Modal Navigation & Link Hinting
 * 2. Hardware-accelerated Dark Mode Engine
 * 3. Consent-O-Matic GDPR Banner Neutralizer
 * 4. Bionic Typography Engine
 * 5. IPC Tab Lifecycle & State Reporting
 */
(function() {
    'use strict';

    if (window.__FeatherEngineLoaded) {
        return;
    }
    window.__FeatherEngineLoaded = true;

    // ==========================================
    // 1. IPC BRIDGE & GLOBAL ENGINE OBJECT
    // ==========================================
    const IPC = {
        send: function(msg) {
            try {
                if (window.ipc && typeof window.ipc.postMessage === 'function') {
                    window.ipc.postMessage(typeof msg === 'string' ? msg : JSON.stringify(msg));
                }
            } catch (e) {
                console.error('[FeatherEngine IPC] Send error:', e);
            }
        }
    };

    window.__FeatherEngine = {
        version: '1.0.0',
        darkModeActive: false,
        bionicActive: false,
        hintModeActive: false,
        vimKeysEnabled: false,

        setDarkMode: function(enabled) {
            DarkModeEngine.toggle(enabled);
            IPC.send({ type: 'set_site_pref', key: 'dark_mode', value: !!enabled });
        },
        toggleDarkMode: function() {
            this.setDarkMode(!this.darkModeActive);
        },
        isDarkMode: function() {
            return this.darkModeActive;
        },

        setBionicReading: function(enabled) {
            BionicEngine.toggle(enabled);
        },
        toggleBionicReading: function() {
            BionicEngine.toggle(!this.bionicActive);
        },
        isBionicReading: function() {
            return this.bionicActive;
        },

        restoreScroll: function(x, y) {
            window.scrollTo({ left: x || 0, top: y || 0, behavior: 'instant' });
        },

        restoreForm: function(data) {
            FormState.restore(data);
        },

        // Called by the host with what this origin has turned on. Sent on load and again
        // whenever the page navigates to a different origin.
        applySitePrefs: function(prefs) {
            prefs = prefs || {};
            this.vimKeysEnabled = prefs.vim_keys === true;
            if (prefs.dark_mode === true && !this.darkModeActive) {
                DarkModeEngine.toggle(true);
            } else if (prefs.dark_mode !== true && this.darkModeActive) {
                DarkModeEngine.toggle(false);
            }
        },

        setVimKeys: function(enabled) {
            this.vimKeysEnabled = !!enabled;
            IPC.send({ type: 'set_site_pref', key: 'vim_keys', value: this.vimKeysEnabled });
        },

        toggleVimKeys: function() {
            this.setVimKeys(!this.vimKeysEnabled);
        },

        isVimKeys: function() {
            return this.vimKeysEnabled;
        },

        navigate: function(url) {
            if (!url) return;
            IPC.send({ type: 'navigate', url: url.trim() });
        },

        sendIpc: function(msg) {
            IPC.send(msg);
        },

        showOmnibar: function() {
            VimNavigation.showOmnibar();
        },

        showFind: function() {
            FindInPage.show();
        },

        toggleVideoPanel: function() {
            if (window.__FeatherVideo) { window.__FeatherVideo.toggle(); }
        },

        updateOmnibarSuggestions: function(suggestions) {
            if (typeof VimNavigation.renderSuggestions === 'function') {
                VimNavigation.renderSuggestions(suggestions);
            }
        },

        reportState: function() {
            IPC.send({
                type: 'scroll_update',
                scrollX: Math.round(window.scrollX || window.pageXOffset || 0),
                scrollY: Math.round(window.scrollY || window.pageYOffset || 0),
                title: document.title || '',
                url: window.location.href,
                formData: FormState.collect()
            });
        }
    };

    // ==========================================
    // 1b. FORM STATE
    // ==========================================
    // Hibernation drops the WebView outright, so there is no moment to ask the page for
    // its form contents on the way out. The page volunteers them with every state report
    // instead, and the host persists whatever it last heard.
    const FormState = {
        // ponytail: fields are keyed by name or id and only simple inputs are kept. No
        // path-based selectors, no contenteditable, no dynamic React-controlled inputs
        // that ignore a value assignment. Widen it when a real site demands it.
        SKIP_TYPES: ['password', 'hidden', 'file', 'submit', 'button', 'image', 'reset'],
        MAX_FIELDS: 40,
        MAX_VALUE: 2000,

        keyFor: function(el) {
            return el.name || el.id || '';
        },

        // Passwords are never collected: this ends up on disk in plain text, in a file
        // that is not the keychain.
        usable: function(el) {
            if (!this.keyFor(el)) return false;
            const type = (el.type || '').toLowerCase();
            if (this.SKIP_TYPES.indexOf(type) !== -1) return false;
            if (el.autocomplete === 'off' && type !== 'checkbox') return false;
            return true;
        },

        collect: function() {
            const out = {};
            let count = 0;
            const fields = document.querySelectorAll('input, textarea, select');
            for (let i = 0; i < fields.length && count < this.MAX_FIELDS; i++) {
                const el = fields[i];
                if (!this.usable(el)) continue;
                const type = (el.type || '').toLowerCase();
                let value;
                if (type === 'checkbox' || type === 'radio') {
                    if (!el.checked) continue;
                    value = true;
                } else {
                    if (!el.value) continue;
                    value = String(el.value).slice(0, this.MAX_VALUE);
                }
                out[this.keyFor(el)] = value;
                count++;
            }
            return count ? out : null;
        },

        restore: function(data) {
            if (!data) return;
            const fields = document.querySelectorAll('input, textarea, select');
            for (let i = 0; i < fields.length; i++) {
                const el = fields[i];
                const key = this.keyFor(el);
                if (!key || !(key in data) || !this.usable(el)) continue;
                const value = data[key];
                const type = (el.type || '').toLowerCase();
                if (type === 'checkbox' || type === 'radio') {
                    el.checked = value === true;
                } else {
                    el.value = value;
                }
                // Frameworks watch events, not assignments.
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
            }
        }
    };

    // ==========================================
    // 2. VIM MODAL NAVIGATION & LINK HINTING
    // ==========================================
    const VimNavigation = {
        lastGTime: 0,
        hintAlphabet: 'asdfjklweruio',
        activeHints: [],
        hintBuffer: '',
        hintOverlay: null,

        // Roles that mean "the user is typing here", beyond the obvious tags. A page that
        // builds its own text field out of a div announces it with one of these.
        EDITABLE_ROLES: ['textbox', 'combobox', 'searchbox', 'spinbutton'],

        isEditable: function(element) {
            if (!element) return false;

            // A focused custom element reports itself as the active element while the real
            // caret is inside its shadow tree. Follow it down.
            let el = element;
            let depth = 0;
            while (el && el.shadowRoot && el.shadowRoot.activeElement && depth < 8) {
                el = el.shadowRoot.activeElement;
                depth++;
            }

            const tag = el.tagName ? el.tagName.toUpperCase() : '';
            if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return true;
            if (el.isContentEditable) return true;

            const role = el.getAttribute && el.getAttribute('role');
            if (role && this.EDITABLE_ROLES.indexOf(role) !== -1) return true;

            // Inside a contenteditable region the caret's node may not itself carry the
            // attribute; the closest ancestor does.
            return !!(el.closest && el.closest('[contenteditable]:not([contenteditable="false"])'));
        },

        init: function() {
            window.addEventListener('keydown', (e) => this.handleKeyDown(e), true);
        },

        handleKeyDown: function(e) {
            // The find bar owns every keystroke while it is open, including inside a
            // page text field: that is what "find" means once you have opened it.
            if (FindInPage.active && FindInPage.handleKey(e)) {
                return;
            }

            // F11 is the cross-platform fullscreen key and belongs to the window, not to
            // whatever the page has focused.
            if (e.key === 'F11') {
                window.__FeatherEngine.sendIpc({ type: 'toggle_fullscreen' });
                e.preventDefault();
                return;
            }

            // Alt belongs to the video layer. Checked before the editable test because a
            // page with a comment box focused is exactly when you want to nudge playback.
            if (e.altKey && window.__FeatherVideo && window.__FeatherVideo.handleKey(e)) {
                return;
            }

            // If link hint mode is active, intercept keystrokes
            if (window.__FeatherEngine.hintModeActive) {
                this.handleHintKey(e);
                return;
            }

            // Command / Control shortcuts should work even if focused in an input
            if (e.metaKey || e.ctrlKey) {
                const key = e.key.toLowerCase();
                if ((e.metaKey && key === 't') || (e.ctrlKey && key === 't')) {
                    window.__FeatherEngine.sendIpc({ type: 'new_tab', url: 'https://example.com' });
                    e.preventDefault();
                    return;
                }
                if ((e.metaKey && key === 'w') || (e.ctrlKey && key === 'w')) {
                    window.__FeatherEngine.sendIpc({ type: 'close_current_tab' });
                    e.preventDefault();
                    return;
                }
                if (e.metaKey && e.altKey) {
                    if (e.key === 'ArrowRight') {
                        window.__FeatherEngine.sendIpc({ type: 'next_tab' });
                        e.preventDefault();
                        return;
                    }
                    if (e.key === 'ArrowLeft') {
                        window.__FeatherEngine.sendIpc({ type: 'prev_tab' });
                        e.preventDefault();
                        return;
                    }
                }
                if (e.ctrlKey && e.key === 'Tab') {
                    if (e.shiftKey) {
                        window.__FeatherEngine.sendIpc({ type: 'prev_tab' });
                    } else {
                        window.__FeatherEngine.sendIpc({ type: 'next_tab' });
                    }
                    e.preventDefault();
                    return;
                }
                if (e.metaKey && e.key >= '1' && e.key <= '9') {
                    const idx = parseInt(e.key, 10) - 1;
                    window.__FeatherEngine.sendIpc({ type: 'switch_tab_index', index: idx });
                    e.preventDefault();
                    return;
                }
                if ((e.metaKey && key === 'l') || (e.ctrlKey && key === 'l')) {
                    this.showOmnibar();
                    e.preventDefault();
                    return;
                }
                // Cmd+Ctrl+F is the system fullscreen shortcut; check it before plain
                // Cmd+F, which is find.
                if (e.metaKey && e.ctrlKey && key === 'f') {
                    window.__FeatherEngine.sendIpc({ type: 'toggle_fullscreen' });
                    e.preventDefault();
                    return;
                }
                if (key === 'f' && !this.isEditable(document.activeElement)) {
                    FindInPage.show();
                    e.preventDefault();
                    return;
                }
                // Cmd+[ / Cmd+] rather than Cmd+arrow: the arrows mean start-of-line
                // inside every text field on the page.
                if (e.key === '[') {
                    history.back();
                    e.preventDefault();
                    return;
                }
                if (e.key === ']') {
                    history.forward();
                    e.preventDefault();
                    return;
                }
                if (e.key === '0') {
                    window.__FeatherEngine.sendIpc({ type: 'zoom', step: 0 });
                    e.preventDefault();
                    return;
                }
                if (e.key === '=' || e.key === '+') {
                    window.__FeatherEngine.sendIpc({ type: 'zoom', step: 1 });
                    e.preventDefault();
                    return;
                }
                if (e.key === '-') {
                    window.__FeatherEngine.sendIpc({ type: 'zoom', step: -1 });
                    e.preventDefault();
                    return;
                }
                return;
            }

            // Ignore shortcuts when editing text, unless pressing Escape
            if (this.isEditable(document.activeElement)) {
                if (e.key === 'Escape') {
                    document.activeElement.blur();
                    e.preventDefault();
                }
                return;
            }

            // Modifier keys bypass Vim shortcuts
            if (e.altKey) {
                return;
            }

            // Single-letter shortcuts are off unless this site was opted in. They swallow
            // bare keystrokes globally, which breaks every site with its own shortcuts —
            // GitHub's "t", Gmail's "x", any site where "f" means something already.
            if (!window.__FeatherEngine.vimKeysEnabled) {
                return;
            }

            switch (e.key) {
                case 'j':
                    window.scrollBy({ top: 60, behavior: 'smooth' });
                    e.preventDefault();
                    break;
                case 'k':
                    window.scrollBy({ top: -60, behavior: 'smooth' });
                    e.preventDefault();
                    break;
                case 'd':
                    window.scrollBy({ top: Math.round(window.innerHeight / 2), behavior: 'smooth' });
                    e.preventDefault();
                    break;
                case 'u':
                    window.scrollBy({ top: -Math.round(window.innerHeight / 2), behavior: 'smooth' });
                    e.preventDefault();
                    break;
                case 'g': {
                    const now = Date.now();
                    if (now - this.lastGTime < 500) {
                        window.scrollTo({ top: 0, behavior: 'smooth' });
                        this.lastGTime = 0;
                    } else {
                        this.lastGTime = now;
                    }
                    e.preventDefault();
                    break;
                }
                case 'G':
                    window.scrollTo({
                        top: Math.max(
                            document.body ? document.body.scrollHeight : 0,
                            document.documentElement ? document.documentElement.scrollHeight : 0
                        ),
                        behavior: 'smooth'
                    });
                    e.preventDefault();
                    break;
                case 'f':
                    this.startHinting();
                    e.preventDefault();
                    break;
                case 'o':
                case 'O':
                    this.showOmnibar();
                    e.preventDefault();
                    break;
                case 't':
                    window.__FeatherEngine.sendIpc({ type: 'new_tab', url: 'https://example.com' });
                    e.preventDefault();
                    break;
                case 'x':
                    window.__FeatherEngine.sendIpc({ type: 'close_current_tab' });
                    e.preventDefault();
                    break;
                case 'H':
                    history.back();
                    e.preventDefault();
                    break;
                case 'L':
                    history.forward();
                    e.preventDefault();
                    break;
                case '/':
                    FindInPage.show();
                    e.preventDefault();
                    break;
                case 'J':
                    window.__FeatherEngine.sendIpc({ type: 'next_tab' });
                    e.preventDefault();
                    break;
                case 'K':
                    window.__FeatherEngine.sendIpc({ type: 'prev_tab' });
                    e.preventDefault();
                    break;
                case 'r':
                    if (e.shiftKey) {
                        window.location.reload();
                        e.preventDefault();
                    }
                    break;
                default:
                    break;
            }
        },

        omnibarSuggestionsEl: null,
        currentSuggestions: [],
        selectedSuggestionIndex: -1,

        renderSuggestions: function(suggestions) {
            if (!this.omnibarSuggestionsEl) return;
            this.currentSuggestions = Array.isArray(suggestions) ? suggestions : [];
            this.selectedSuggestionIndex = -1;
            this.omnibarSuggestionsEl.innerHTML = '';

            if (this.currentSuggestions.length === 0) {
                this.omnibarSuggestionsEl.style.display = 'none';
                return;
            }

            this.omnibarSuggestionsEl.style.display = 'flex';
            this.currentSuggestions.forEach((item, index) => {
                const row = document.createElement('div');
                row.className = 'feather-omnibar-row';
                row.dataset.index = index;
                row.style.cssText = 'display: flex; align-items: center; gap: 10px; padding: 7px 10px; border-radius: 6px; cursor: pointer; transition: background 0.1s; user-select: none; font-size: 13px;';

                const icon = document.createElement('span');
                icon.textContent = item.icon || '🔍';
                icon.style.cssText = 'font-size: 14px; width: 20px; text-align: center; flex-shrink: 0;';

                const textCol = document.createElement('div');
                textCol.style.cssText = 'flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px;';

                const title = document.createElement('span');
                title.textContent = item.title;
                title.style.cssText = 'color: #e6e6f0; font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 13px;';

                const url = document.createElement('span');
                url.textContent = item.url;
                url.style.cssText = 'color: #7b7b92; font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;';

                textCol.appendChild(title);
                textCol.appendChild(url);

                const badge = document.createElement('span');
                badge.textContent = item.kind ? item.kind.toUpperCase() : 'SEARCH';
                badge.style.cssText = 'font-size: 9px; font-weight: 700; color: #8c8ca8; background: #262634; padding: 2px 6px; border-radius: 4px; flex-shrink: 0; letter-spacing: 0.5px; border: 1px solid #36364a;';

                row.appendChild(icon);
                row.appendChild(textCol);
                row.appendChild(badge);

                row.onmouseenter = () => {
                    this.setSuggestionHighlight(index);
                };
                row.onclick = (e) => {
                    e.stopPropagation();
                    const overlay = document.getElementById('feather-omnibar-overlay');
                    if (overlay && overlay.parentNode) overlay.parentNode.removeChild(overlay);
                    this.omnibarSuggestionsEl = null;
                    this.currentSuggestions = [];
                    this.selectedSuggestionIndex = -1;
                    window.__FeatherEngine.navigate(item.url);
                };

                this.omnibarSuggestionsEl.appendChild(row);
            });
        },

        setSuggestionHighlight: function(index) {
            if (!this.omnibarSuggestionsEl) return;
            this.selectedSuggestionIndex = index;
            const rows = this.omnibarSuggestionsEl.querySelectorAll('.feather-omnibar-row');
            rows.forEach((r, i) => {
                if (i === index) {
                    r.style.background = '#323244';
                    r.style.boxShadow = '0 0 0 1px #4e4e66 inset';
                } else {
                    r.style.background = 'transparent';
                    r.style.boxShadow = 'none';
                }
            });
        },

        showOmnibar: function() {
            if (document.getElementById('feather-omnibar-overlay')) {
                return;
            }
            const overlay = document.createElement('div');
            overlay.id = 'feather-omnibar-overlay';
            overlay.style.cssText = 'position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; background: rgba(0,0,0,0.65); z-index: 2147483647; display: flex; justify-content: center; align-items: flex-start; padding-top: 14vh; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; backdrop-filter: blur(4px);';

            const card = document.createElement('div');
            card.style.cssText = 'background: #18181f; border: 1px solid #383846; border-radius: 12px; width: 660px; max-width: 92vw; padding: 14px; box-shadow: 0 20px 50px rgba(0,0,0,0.85);';

            const label = document.createElement('div');
            label.style.cssText = 'color: #8e8ea6; font-size: 11px; font-weight: 600; text-transform: uppercase; margin-bottom: 8px; letter-spacing: 0.6px; display: flex; justify-content: space-between;';
            label.innerHTML = '<span>Address or Privacy Search</span><span style="color: #616179;">↑↓ Navigate • Tab Fill • Enter Go • Esc Close</span>';

            const input = document.createElement('input');
            input.type = 'text';
            input.placeholder = 'Search DuckDuckGo or type URL (e.g. !gh wry, news.ycombinator.com)...';
            const currentUrl = window.location.href;
            input.value = (currentUrl === 'about:blank' || !currentUrl) ? '' : currentUrl;
            input.style.cssText = 'width: 100%; box-sizing: border-box; background: #22222c; color: #fff; border: 1px solid #48485e; border-radius: 8px; padding: 11px 14px; font-size: 15px; outline: none; transition: border-color 0.15s;';

            input.onfocus = function() {
                input.style.borderColor = '#6c6c92';
                input.select();
            };
            input.onblur = function() {
                input.style.borderColor = '#48485e';
            };

            const suggestionsBox = document.createElement('div');
            suggestionsBox.id = 'feather-omnibar-suggestions';
            suggestionsBox.style.cssText = 'margin-top: 8px; display: none; flex-direction: column; gap: 3px; max-height: 320px; overflow-y: auto;';
            this.omnibarSuggestionsEl = suggestionsBox;

            let queryDebounce = null;
            const triggerQuery = (val) => {
                if (queryDebounce) clearTimeout(queryDebounce);
                queryDebounce = setTimeout(() => {
                    window.__FeatherEngine.sendIpc({ type: 'omnibar_query', query: val });
                }, 40);
            };

            input.oninput = function() {
                triggerQuery(input.value);
            };

            input.onkeydown = (ev) => {
                if (ev.key === 'ArrowDown') {
                    ev.preventDefault();
                    if (this.currentSuggestions.length > 0) {
                        const nextIdx = (this.selectedSuggestionIndex + 1) % this.currentSuggestions.length;
                        this.setSuggestionHighlight(nextIdx);
                    }
                } else if (ev.key === 'ArrowUp') {
                    ev.preventDefault();
                    if (this.currentSuggestions.length > 0) {
                        const prevIdx = this.selectedSuggestionIndex <= 0
                            ? this.currentSuggestions.length - 1
                            : this.selectedSuggestionIndex - 1;
                        this.setSuggestionHighlight(prevIdx);
                    }
                } else if (ev.key === 'Tab') {
                    ev.preventDefault();
                    if (this.selectedSuggestionIndex >= 0 && this.selectedSuggestionIndex < this.currentSuggestions.length) {
                        const sel = this.currentSuggestions[this.selectedSuggestionIndex];
                        input.value = sel.url;
                    }
                } else if (ev.key === 'Enter') {
                    ev.preventDefault();
                    let target = input.value;
                    if (this.selectedSuggestionIndex >= 0 && this.selectedSuggestionIndex < this.currentSuggestions.length) {
                        target = this.currentSuggestions[this.selectedSuggestionIndex].url;
                    }
                    if (overlay.parentNode) overlay.parentNode.removeChild(overlay);
                    this.omnibarSuggestionsEl = null;
                    this.currentSuggestions = [];
                    this.selectedSuggestionIndex = -1;
                    window.__FeatherEngine.navigate(target);
                } else if (ev.key === 'Escape') {
                    ev.preventDefault();
                    if (overlay.parentNode) overlay.parentNode.removeChild(overlay);
                    this.omnibarSuggestionsEl = null;
                    this.currentSuggestions = [];
                    this.selectedSuggestionIndex = -1;
                }
            };

            overlay.onclick = (ev) => {
                if (ev.target === overlay) {
                    if (overlay.parentNode) overlay.parentNode.removeChild(overlay);
                    this.omnibarSuggestionsEl = null;
                    this.currentSuggestions = [];
                    this.selectedSuggestionIndex = -1;
                }
            };

            card.appendChild(label);
            card.appendChild(input);
            card.appendChild(suggestionsBox);
            overlay.appendChild(card);
            document.documentElement.appendChild(overlay);
            input.focus();

            // Immediately query to populate recent history suggestions
            triggerQuery(input.value);
        },

        startHinting: function() {
            this.clearHints();
            const elements = this.findClickableElements();
            if (elements.length === 0) return;

            window.__FeatherEngine.hintModeActive = true;
            this.hintBuffer = '';

            const chords = this.generateChords(elements.length);
            const overlay = document.createElement('div');
            overlay.id = 'feather-hint-overlay';
            overlay.style.cssText = 'position: absolute; top: 0; left: 0; width: 100%; height: 100%; pointer-events: none; z-index: 2147483647;';

            this.activeHints = elements.map((elem, idx) => {
                const rect = elem.getBoundingClientRect();
                const badge = document.createElement('span');
                badge.className = 'feather-hint-badge';
                badge.textContent = chords[idx].toUpperCase();
                badge.style.cssText = 
                    'position: fixed; top: ' + Math.max(0, Math.round(rect.top)) + 'px; left: ' + Math.max(0, Math.round(rect.left)) + 'px; ' +
                    'background: #fffb00 !important; color: #000000 !important; font-family: monospace !important; ' +
                    'font-size: 11px !important; font-weight: bold !important; line-height: 1 !important; ' +
                    'padding: 2px 4px !important; border: 1px solid #111 !important; border-radius: 3px !important; ' +
                    'box-shadow: 0 1px 4px rgba(0,0,0,0.6) !important; z-index: 2147483647; text-transform: uppercase;';

                overlay.appendChild(badge);
                return { chord: chords[idx], element: elem, badge: badge };
            });

            document.documentElement.appendChild(overlay);
            this.hintOverlay = overlay;
        },

        clearHints: function() {
            window.__FeatherEngine.hintModeActive = false;
            this.hintBuffer = '';
            this.activeHints = [];
            if (this.hintOverlay && this.hintOverlay.parentNode) {
                this.hintOverlay.parentNode.removeChild(this.hintOverlay);
            }
            this.hintOverlay = null;
        },

        handleHintKey: function(e) {
            e.preventDefault();
            e.stopPropagation();

            if (e.key === 'Escape') {
                this.clearHints();
                return;
            }

            const char = e.key.toLowerCase();
            if (!this.hintAlphabet.includes(char)) {
                return;
            }

            this.hintBuffer += char;
            const exactMatch = this.activeHints.find(h => h.chord === this.hintBuffer);
            if (exactMatch) {
                const target = exactMatch.element;
                this.clearHints();
                if (typeof target.focus === 'function') target.focus();
                if (typeof target.click === 'function') target.click();
                return;
            }

            // Filter remaining hints
            let matches = 0;
            this.activeHints.forEach(h => {
                if (h.chord.startsWith(this.hintBuffer)) {
                    matches++;
                    h.badge.style.opacity = '1';
                } else {
                    h.badge.style.opacity = '0.2';
                }
            });

            if (matches === 0) {
                this.clearHints();
            }
        },

        findClickableElements: function() {
            const candidates = Array.from(document.querySelectorAll(
                'a[href], button, input:not([type="hidden"]), select, textarea, [role="button"], [role="link"], [tabindex]:not([tabindex="-1"])'
            ));

            const winH = window.innerHeight;
            const winW = window.innerWidth;

            return candidates.filter(el => {
                const rect = el.getBoundingClientRect();
                const isVisible = (
                    rect.width > 0 &&
                    rect.height > 0 &&
                    rect.top < winH &&
                    rect.bottom > 0 &&
                    rect.left < winW &&
                    rect.right > 0
                );
                if (!isVisible) return false;
                const style = window.getComputedStyle(el);
                return style.visibility !== 'hidden' && style.display !== 'none' && style.opacity !== '0';
            });
        },

        generateChords: function(count) {
            const alpha = this.hintAlphabet;
            const chords = [];
            if (count <= alpha.length) {
                for (let i = 0; i < count; i++) {
                    chords.push(alpha[i]);
                }
            } else {
                for (let i = 0; i < alpha.length && chords.length < count; i++) {
                    for (let j = 0; j < alpha.length && chords.length < count; j++) {
                        chords.push(alpha[i] + alpha[j]);
                    }
                }
            }
            return chords;
        }
    };

    // ==========================================
    // 3. DARK MODE ENGINE
    // ==========================================
    const DarkModeEngine = {
        styleId: 'feather-dark-reader-style',
        cssRules: 
            'html { filter: invert(90%) hue-rotate(180deg) !important; background: #121212 !important; }\n' +
            'img, video, canvas, svg, [style*="background-image"] { filter: invert(100%) hue-rotate(180deg) !important; }\n' +
            'iframe { filter: invert(0%) !important; }\n',

        toggle: function(enable) {
            let style = document.getElementById(this.styleId);
            if (enable) {
                if (!style) {
                    style = document.createElement('style');
                    style.id = this.styleId;
                    style.textContent = this.cssRules;
                    (document.head || document.documentElement).appendChild(style);
                }
                window.__FeatherEngine.darkModeActive = true;
            } else {
                if (style && style.parentNode) {
                    style.parentNode.removeChild(style);
                }
                window.__FeatherEngine.darkModeActive = false;
            }
        }
    };

    // ==========================================
    // 4. CONSENT-O-MATIC BANNER NEUTRALIZER
    // ==========================================
    const BannerNeutralizer = {
        selectors: [
            '#onetrust-banner-sdk',
            '#onetrust-consent-sdk',
            '.cc-window',
            '#cmpContainer',
            '.cookie-banner',
            '[class*="cookie-banner" i]',
            '[id*="cookie-banner" i]',
            '[id*="consent-banner" i]',
            '[class*="consent-banner" i]',
            '.qc-cmp2-container',
            '#CybotCookiebotDialog',
            '[id*="cookie-modal" i]',
            '[class*="cookie-modal" i]',
            '.cookie-alert',
            '#gdpr-cookie-message'
        ],

        // One selector string, so a sweep is one querySelectorAll instead of sixteen.
        query: null,
        pending: false,

        clean: function() {
            if (!this.query) {
                this.query = this.selectors.join(',');
            }

            let removedCount = 0;
            const nodes = document.querySelectorAll(this.query);
            for (let i = 0; i < nodes.length; i++) {
                const node = nodes[i];
                if (node && node.parentNode) {
                    node.parentNode.removeChild(node);
                    removedCount++;
                }
            }

            // Only after actually removing a banner. Forcing overflow and position on every
            // sweep overrode the site's own layout on pages that never had a banner at all.
            if (removedCount > 0) {
                if (document.body) {
                    document.body.style.setProperty('overflow', 'auto', 'important');
                    document.body.style.setProperty('position', 'static', 'important');
                }
                if (document.documentElement) {
                    document.documentElement.style.setProperty('overflow', 'auto', 'important');
                }
            }
        },

        // A mutation only marks the document dirty; the sweep happens once before the next
        // paint. Cleaning inside the observer meant a full-document scan per mutation, and
        // an app that mutates in a loop got thousands of scans a second — for a banner that
        // is either there in the first second or never.
        schedule: function() {
            if (this.pending) return;
            this.pending = true;
            const self = this;
            requestAnimationFrame(function() {
                self.pending = false;
                self.clean();
            });
        },

        init: function() {
            this.clean();
            const self = this;
            const observer = new MutationObserver(function() { self.schedule(); });
            observer.observe(document.documentElement || document, {
                childList: true,
                subtree: true
            });
        }
    };

    // ==========================================
    // 5. BIONIC TYPOGRAPHY ENGINE
    // ==========================================
    const BionicEngine = {
        processedClass: 'feather-bionic-processed',

        toggle: function(enable) {
            window.__FeatherEngine.bionicActive = enable;
            if (enable) {
                this.apply();
            } else {
                this.remove();
            }
        },

        apply: function() {
            const root = document.body || document.documentElement;
            if (!root) return;

            const walker = document.createTreeWalker(
                root,
                NodeFilter.SHOW_TEXT,
                {
                    acceptNode: function(node) {
                        if (!node.nodeValue || !node.nodeValue.trim()) return NodeFilter.FILTER_REJECT;
                        const parent = node.parentElement;
                        if (!parent) return NodeFilter.FILTER_REJECT;
                        const tag = parent.tagName.toUpperCase();
                        if (
                            tag === 'SCRIPT' ||
                            tag === 'STYLE' ||
                            tag === 'NOSCRIPT' ||
                            tag === 'PRE' ||
                            tag === 'CODE' ||
                            tag === 'TEXTAREA' ||
                            tag === 'INPUT' ||
                            parent.classList.contains('feather-bionic-span')
                        ) {
                            return NodeFilter.FILTER_REJECT;
                        }
                        return NodeFilter.FILTER_ACCEPT;
                    }
                }
            );

            const nodesToTransform = [];
            while (walker.nextNode()) {
                nodesToTransform.push(walker.currentNode);
            }

            nodesToTransform.forEach(textNode => {
                const text = textNode.nodeValue;
                if (!text || text.length < 2) return;

                const parent = textNode.parentNode;
                if (!parent) return;

                const span = document.createElement('span');
                span.className = 'feather-bionic-span';
                span.setAttribute('data-original', text);

                // Built as nodes, never as an HTML string: this text came off the page,
                // and a page that displays escaped markup would otherwise have it parsed
                // back into live elements here.
                const tokenPattern = /[a-zA-Z0-9]+/g;
                let cursor = 0;
                let match;
                while ((match = tokenPattern.exec(text)) !== null) {
                    if (match.index > cursor) {
                        span.appendChild(document.createTextNode(text.slice(cursor, match.index)));
                    }
                    const token = match[0];
                    if (token.length <= 1) {
                        span.appendChild(document.createTextNode(token));
                    } else {
                        const mid = Math.ceil(token.length / 2);
                        const bold = document.createElement('b');
                        bold.textContent = token.slice(0, mid);
                        span.appendChild(bold);
                        span.appendChild(document.createTextNode(token.slice(mid)));
                    }
                    cursor = match.index + token.length;
                }
                if (cursor < text.length) {
                    span.appendChild(document.createTextNode(text.slice(cursor)));
                }

                parent.replaceChild(span, textNode);
            });
        },

        remove: function() {
            const spans = Array.from(document.querySelectorAll('.feather-bionic-span'));
            spans.forEach(span => {
                const original = span.getAttribute('data-original');
                if (original !== null && span.parentNode) {
                    const textNode = document.createTextNode(original);
                    span.parentNode.replaceChild(textNode, span);
                }
            });
        }
    };

    // ==========================================
    // 5b. FIND IN PAGE
    // ==========================================
    // WebKit still implements window.find(), which does the selection, scrolling and
    // wrapping that a hand-rolled DOM walk would have to reimplement badly.
    // ponytail: window.find gives no match count, so there is no "3 of 17" readout. It
    // would need a TreeWalker and range highlighting; add that when someone misses it.
    const FindInPage = {
        host: null,
        bar: null,
        field: null,
        query: '',
        active: false,

        show: function() {
            if (this.active) {
                this.render();
                return;
            }
            // The bar lives in a shadow root because window.find searches the rendered
            // document, our own query text included — it would match what you just typed.
            const host = document.createElement('div');
            host.id = 'feather-find-host';
            const root = host.attachShadow({ mode: 'open' });

            const bar = document.createElement('div');
            bar.id = 'feather-find-bar';
            bar.style.cssText = 'position: fixed; top: 12px; right: 12px; z-index: 2147483646; display: flex; align-items: center; gap: 8px; background: #1c1c26; border: 1px solid #33334a; border-radius: 8px; padding: 7px 10px; box-shadow: 0 8px 24px rgba(0,0,0,0.45); font: 13px -apple-system, system-ui, sans-serif; color: #e6e6f0;';

            const field = document.createElement('span');
            field.style.cssText = 'min-width: 160px; border-bottom: 1px solid #3a3a52; padding-bottom: 1px;';

            const hint = document.createElement('span');
            hint.textContent = '⏎ next · ⇧⏎ prev · esc';
            hint.style.cssText = 'color: #6f6f88; font-size: 11px;';

            bar.appendChild(field);
            bar.appendChild(hint);
            root.appendChild(bar);
            (document.body || document.documentElement).appendChild(host);

            this.host = host;
            this.bar = bar;
            this.field = field;
            this.query = '';
            this.active = true;
            this.render();
        },

        render: function(missed) {
            this.field.textContent = this.query || 'Find in page';
            this.field.style.color = missed ? '#ff8080' : (this.query ? '#e6e6f0' : '#6f6f88');
        },

        // Nothing in the bar takes focus. A focused field would own the document
        // selection, and window.find restarts from wherever the selection is — stepping
        // through matches only works while the matches themselves hold it.
        // ponytail: so no paste, no caret, no IME in the query. Keystrokes and backspace.
        handleKey: function(e) {
            if (e.key === 'Escape') {
                this.hide();
            } else if (e.key === 'Enter') {
                this.step(e.shiftKey);
            } else if (e.key === 'Backspace') {
                this.query = this.query.slice(0, -1);
                this.restart();
            } else if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
                this.query += e.key;
                this.restart();
            } else {
                return false;
            }
            e.preventDefault();
            e.stopPropagation();
            return true;
        },

        /// A changed query searches from the top again, so the first match is the first one.
        restart: function() {
            const sel = window.getSelection();
            if (sel) sel.removeAllRanges();
            this.step(false);
        },

        step: function(backwards) {
            if (!this.query) {
                this.render(false);
                return;
            }
            let found = false;
            try {
                found = window.find(this.query, false, !!backwards, true, false, true, false);
            } catch (e) {
                found = false;
            }
            this.render(!found);
        },

        hide: function() {
            if (!this.active) return;
            if (this.host && this.host.parentNode) this.host.parentNode.removeChild(this.host);
            this.host = null;
            this.bar = null;
            this.field = null;
            this.query = '';
            this.active = false;
        }
    };

    // ==========================================
    // 5c. LINK STATUS & LINK CONTEXT MENU
    // ==========================================
    const LinkChrome = {
        status: null,
        menu: null,

        linkAt: function(node) {
            return node && node.closest ? node.closest('a[href]') : null;
        },

        showStatus: function(href) {
            if (!this.status) {
                const el = document.createElement('div');
                el.id = 'feather-link-status';
                el.style.cssText = 'position: fixed; bottom: 0; left: 0; z-index: 2147483645; max-width: 70vw; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; background: #14141c; color: #c7c7da; border: 1px solid #2b2b3c; border-left: none; border-bottom: none; border-radius: 0 6px 0 0; padding: 3px 8px; font: 11px -apple-system, system-ui, sans-serif; pointer-events: none;';
                (document.body || document.documentElement).appendChild(el);
                this.status = el;
            }
            this.status.textContent = href;
            this.status.style.display = 'block';
        },

        hideStatus: function() {
            if (this.status) this.status.style.display = 'none';
        },

        closeMenu: function() {
            if (this.menu && this.menu.parentNode) this.menu.parentNode.removeChild(this.menu);
            this.menu = null;
        },

        copy: function(text) {
            if (navigator.clipboard && navigator.clipboard.writeText) {
                navigator.clipboard.writeText(text).catch(() => this.copyFallback(text));
            } else {
                this.copyFallback(text);
            }
        },

        // Pages served over http have no navigator.clipboard at all.
        copyFallback: function(text) {
            const ta = document.createElement('textarea');
            ta.value = text;
            ta.style.cssText = 'position: fixed; opacity: 0;';
            (document.body || document.documentElement).appendChild(ta);
            ta.select();
            try { document.execCommand('copy'); } catch (e) {}
            ta.remove();
        },

        showMenu: function(x, y, href) {
            this.closeMenu();
            const menu = document.createElement('div');
            menu.style.cssText = 'position: fixed; z-index: 2147483647; min-width: 180px; background: #1c1c26; border: 1px solid #33334a; border-radius: 8px; padding: 4px; box-shadow: 0 10px 30px rgba(0,0,0,0.5); font: 13px -apple-system, system-ui, sans-serif;';
            menu.style.left = Math.min(x, window.innerWidth - 200) + 'px';
            menu.style.top = Math.min(y, window.innerHeight - 90) + 'px';

            const item = (label, fn) => {
                const row = document.createElement('div');
                row.textContent = label;
                row.style.cssText = 'padding: 7px 10px; border-radius: 5px; color: #e6e6f0; cursor: pointer; user-select: none;';
                row.onmouseenter = () => { row.style.background = '#2e2e40'; };
                row.onmouseleave = () => { row.style.background = 'transparent'; };
                row.onclick = () => { this.closeMenu(); fn(); };
                menu.appendChild(row);
            };

            item('Open in new tab', () => {
                window.__FeatherEngine.sendIpc({ type: 'new_tab', url: href });
            });
            item('Copy link', () => this.copy(href));

            (document.body || document.documentElement).appendChild(menu);
            this.menu = menu;
        },

        init: function() {
            document.addEventListener('mouseover', (e) => {
                const link = this.linkAt(e.target);
                if (link && link.href) {
                    this.showStatus(link.href);
                } else {
                    this.hideStatus();
                }
            }, true);
            window.addEventListener('blur', () => this.hideStatus());

            document.addEventListener('contextmenu', (e) => {
                const link = this.linkAt(e.target);
                // No link under the cursor means WebKit's own menu, which already does
                // copy, look up and inspect better than a div can.
                if (!link || !link.href) return;
                e.preventDefault();
                this.showMenu(e.clientX, e.clientY, link.href);
            }, true);

            document.addEventListener('mousedown', (e) => {
                if (this.menu && !this.menu.contains(e.target)) this.closeMenu();
            }, true);
            window.addEventListener('keydown', (e) => {
                if (e.key === 'Escape') this.closeMenu();
            }, true);
            window.addEventListener('scroll', () => this.closeMenu(), { passive: true });
        }
    };

    // ==========================================
    // 6. SCROLL & LIFECYCLE MONITORING
    // ==========================================
    let scrollDebounceTimer = null;
    function handleScrollEvent() {
        if (scrollDebounceTimer) {
            clearTimeout(scrollDebounceTimer);
        }
        scrollDebounceTimer = setTimeout(function() {
            window.__FeatherEngine.reportState();
        }, 150);
    }

    // ==========================================
    // 6. SINGLE-PAGE APP NAVIGATION
    // ==========================================
    // An SPA changes the page without a load event, so without this the tab strip keeps the
    // title it had at boot and a hibernated tab rehydrates to the URL the user started at
    // rather than the one they were reading.
    const SpaWatcher = {
        lastUrl: location.href,
        lastOrigin: location.origin,

        changed: function() {
            if (location.href === this.lastUrl) return;
            this.lastUrl = location.href;

            // Leaving the origin means different site preferences apply.
            if (location.origin !== this.lastOrigin) {
                this.lastOrigin = location.origin;
                IPC.send({ type: 'site_prefs_request' });
            }

            // The framework usually sets document.title just after the URL, not before.
            setTimeout(function() {
                window.__FeatherEngine.reportState();
            }, 60);
        },

        init: function() {
            const self = this;
            ['pushState', 'replaceState'].forEach(function(name) {
                const original = history[name];
                if (typeof original !== 'function') return;
                history[name] = function() {
                    const result = original.apply(this, arguments);
                    self.changed();
                    return result;
                };
            });
            window.addEventListener('popstate', function() { self.changed(); });
            window.addEventListener('hashchange', function() { self.changed(); });
        }
    };

    // Initialize subsystems
    VimNavigation.init();
    BannerNeutralizer.init();
    SpaWatcher.init();
    LinkChrome.init();
    IPC.send({ type: 'site_prefs_request' });

    window.addEventListener('scroll', handleScrollEvent, { passive: true });
    window.addEventListener('load', function() {
        window.__FeatherEngine.reportState();
    });

    console.log('[FeatherEngine] Runtime.js successfully loaded.');
})();
