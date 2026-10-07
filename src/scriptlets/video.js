/**
 * FeatherBrowser — video layer.
 *
 * Injected into EVERY frame, unlike runtime.js, which WebKit gives to the main frame
 * only. On a streaming site the <video> is almost never in the page you navigated to:
 * it is inside a cross-origin player iframe that the top frame cannot even read. So the
 * controls have to live in whichever frame the video does, and the top frame forwards
 * to it by postMessage.
 */
(function() {
    'use strict';
    if (window.__FeatherVideo) return;

const VideoLayer = {
        panel: null,
        host: null,
        chains: new WeakMap(),
        hud: null,
        hudTimer: null,

        // The video worth controlling: the one that is playing, else the biggest one.
        // Looked up per keystroke rather than tracked, so a player swapped in by a script
        // needs no observer to be found.
        active: function() {
            const all = Array.from(document.querySelectorAll('video'))
                .filter(v => v.offsetWidth > 0 || v.currentTime > 0);
            if (!all.length) return null;
            const playing = all.filter(v => !v.paused && !v.ended);
            const pool = playing.length ? playing : all;
            return pool.sort((a, b) => (b.offsetWidth * b.offsetHeight) - (a.offsetWidth * a.offsetHeight))[0];
        },

        isTop: window.top === window,

        // Hand a command down to every child frame. Returns whether there was anywhere to
        // send it: that is how a frame decides between forwarding and reporting no video.
        broadcast: function(msg) {
            const frames = document.querySelectorAll('iframe, frame');
            let sent = 0;
            for (const f of frames) {
                try {
                    f.contentWindow.postMessage(
                        { __feather: 'video', cmd: msg.cmd, key: msg.key, on: msg.on }, '*'
                    );
                    sent++;
                } catch (e) {}
            }
            return sent > 0;
        },

        // ---- playback state ----
        // The host needs to know a tab is playing so it does not hibernate it, and the tab
        // strip needs it to draw the speaker. The video is usually several frames down, so
        // each frame reports its own state up and the top frame sends the total over IPC.
        childState: new Map(),
        lastSent: null,

        localPlaying: function() {
            for (const v of document.querySelectorAll('video, audio')) {
                if (!v.paused && !v.ended && v.readyState > 2) return v;
            }
            return null;
        },

        report: function() {
            const local = this.localPlaying();
            let playing = !!local;
            let muted = local ? (local.muted || local.volume === 0) : true;
            for (const st of this.childState.values()) {
                if (st.playing) {
                    playing = true;
                    muted = muted && st.muted;
                }
            }
            const state = { playing: playing, muted: playing ? muted : false };
            const same = this.lastSent
                && this.lastSent.playing === state.playing
                && this.lastSent.muted === state.muted;
            if (same) return;
            this.lastSent = state;

            if (this.isTop) {
                try {
                    window.ipc.postMessage(JSON.stringify({
                        type: 'media_state', playing: state.playing, muted: state.muted
                    }));
                } catch (e) {}
            } else {
                try {
                    window.parent.postMessage({
                        __feather: 'video', cmd: 'state',
                        playing: state.playing, muted: state.muted
                    }, '*');
                } catch (e) {}
            }
        },

        // Media events do not bubble, but a capture listener on the document still sees
        // them on the way down, which is cheaper than finding every element first.
        watch: function() {
            const self = this;
            let pending = null;
            const ping = function() {
                clearTimeout(pending);
                pending = setTimeout(function() { self.report(); }, 250);
            };
            ['play', 'playing', 'pause', 'ended', 'volumechange', 'emptied', 'loadeddata']
                .forEach(function(t) { document.addEventListener(t, ping, true); });
            // A frame that is torn down leaves its parent believing it is still playing.
            window.addEventListener('pagehide', function() {
                self.lastSent = null;
                self.childState.clear();
                self.report();
            });
            ping();
        },

        setMuted: function(on) {
            let hit = false;
            for (const v of document.querySelectorAll('video, audio')) {
                v.muted = on;
                hit = true;
            }
            this.broadcast({ cmd: 'mute', on: on });
            if (hit) this.report();
        },

        // Seek to where this page was left, but only if playback has just begun: someone
        // who already scrubbed somewhere does not want to be thrown back.
        resumeAt: function(seconds) {
            const video = this.active();
            if (!video) return this.broadcast({ cmd: 'resume', key: seconds });
            if (!(seconds > 60)) return true;
            if (video.currentTime > 10) return true;
            if (video.duration && seconds > video.duration - 60) return true;
            video.currentTime = seconds;
            const m = Math.floor(seconds / 60), sec = Math.floor(seconds % 60);
            this.toast('Resumed at ' + m + ':' + (sec < 10 ? '0' : '') + sec);
            return true;
        },

        progress: function() {
            const v = this.localPlaying();
            if (v && v.duration && isFinite(v.duration)) {
                return { position: Math.floor(v.currentTime), duration: Math.floor(v.duration) };
            }
            for (const st of this.childState.values()) {
                if (st.position) return { position: st.position, duration: st.duration };
            }
            return null;
        },

        toast: function(text) {
            if (!this.hud) {
                const el = document.createElement('div');
                el.style.cssText = 'position: fixed; left: 50%; top: 12%; transform: translateX(-50%); z-index: 2147483644; background: rgba(12,12,18,0.88); color: #fff; border-radius: 8px; padding: 8px 16px; font: 600 15px -apple-system, system-ui, sans-serif; pointer-events: none; transition: opacity 0.2s;';
                (document.body || document.documentElement).appendChild(el);
                this.hud = el;
            }
            this.hud.textContent = text;
            this.hud.style.opacity = '1';
            clearTimeout(this.hudTimer);
            this.hudTimer = setTimeout(() => { if (this.hud) this.hud.style.opacity = '0'; }, 1100);
        },

        // ---- audio ----
        // createMediaElementSource on a cross-origin resource routes the element through
        // Web Audio and outputs silence, permanently — there is no undo once the source
        // node exists. So the chain is only ever built for media we know is same-origin:
        // blob: and MSE sources (which is what every streaming player uses), data:, our
        // own origin, or an element that opted into CORS.
        audioIsSafe: function(video) {
            const src = video.currentSrc || video.src || '';
            if (!src) return false;
            if (video.crossOrigin) return true;
            if (/^(blob:|data:|mediasource:)/.test(src)) return true;
            try { return new URL(src, location.href).origin === location.origin; }
            catch (e) { return false; }
        },

        chain: function(video) {
            if (this.chains.has(video)) return this.chains.get(video);
            if (!this.audioIsSafe(video)) return null;
            try {
                const Ctx = window.AudioContext || window.webkitAudioContext;
                const ctx = new Ctx();
                const source = ctx.createMediaElementSource(video);
                const bands = [
                    { type: 'lowshelf', frequency: 200 },
                    { type: 'peaking', frequency: 1000, Q: 1 },
                    { type: 'highshelf', frequency: 4000 }
                ].map(spec => {
                    const f = ctx.createBiquadFilter();
                    f.type = spec.type;
                    f.frequency.value = spec.frequency;
                    if (spec.Q) f.Q.value = spec.Q;
                    f.gain.value = 0;
                    return f;
                });
                const comp = ctx.createDynamicsCompressor();
                comp.threshold.value = 0;   // inert until the compressor is switched on
                const gain = ctx.createGain();
                const splitter = ctx.createChannelSplitter(2);
                const merger = ctx.createChannelMerger(2);

                source.connect(bands[0]);
                bands[0].connect(bands[1]);
                bands[1].connect(bands[2]);
                bands[2].connect(comp);
                comp.connect(gain);
                gain.connect(ctx.destination);

                const c = { ctx, source, bands, comp, gain, splitter, merger, mono: false };
                this.chains.set(video, c);
                return c;
            } catch (e) {
                return null;
            }
        },

        setGain: function(video, value) {
            const c = this.chain(video);
            if (!c) {
                this.toast('Audio effects unavailable (cross-origin media)');
                return null;
            }
            if (c.ctx.state === 'suspended') c.ctx.resume();
            c.gain.gain.value = value;
            return c;
        },

        setMono: function(video, on) {
            const c = this.chain(video);
            if (!c) return;
            try {
                c.gain.disconnect();
                if (on) {
                    // Both output channels fed from the same input channel: what a mono
                    // downmix is, and the reason to want one is a badly mixed stereo track.
                    c.gain.connect(c.splitter);
                    c.splitter.connect(c.merger, 0, 0);
                    c.splitter.connect(c.merger, 0, 1);
                    c.merger.connect(c.ctx.destination);
                } else {
                    try { c.splitter.disconnect(); c.merger.disconnect(); } catch (e) {}
                    c.gain.connect(c.ctx.destination);
                }
                c.mono = on;
            } catch (e) {}
        },

        // Only the top frame has the IPC bridge, so a request from a player frame walks up
        // the same way playback state does.
        requestDownload: function(url) {
            if (this.isTop) {
                try {
                    window.ipc.postMessage(JSON.stringify({
                        type: 'download_media', url: url, referer: location.href
                    }));
                } catch (e) {}
            } else {
                try {
                    window.parent.postMessage({
                        __feather: 'video', cmd: 'download', key: url, referer: location.href
                    }, '*');
                } catch (e) {}
            }
        },

        // ---- picture ----
        picture: { brightness: 1, contrast: 1, saturate: 1, grayscale: 0 },

        applyPicture: function(video) {
            const p = this.picture;
            const parts = [];
            if (p.brightness !== 1) parts.push('brightness(' + p.brightness.toFixed(2) + ')');
            if (p.contrast !== 1) parts.push('contrast(' + p.contrast.toFixed(2) + ')');
            if (p.saturate !== 1) parts.push('saturate(' + p.saturate.toFixed(2) + ')');
            if (p.grayscale) parts.push('grayscale(1)');
            video.style.filter = parts.join(' ');
        },

        // ---- transport ----
        seek: function(video, delta) {
            video.currentTime = Math.max(0, video.currentTime + delta);
            this.toast((delta > 0 ? '+' : '') + delta + 's');
        },

        frameStep: function(video, dir) {
            video.pause();
            // No API reports the real frame rate, so this is a 30 fps assumption. It steps
            // by a fixed amount; on 24 or 60 fps content that is not exactly one frame.
            // ponytail: requestVideoFrameCallback would give the true cadence. Add it when
            // stepping accuracy matters more than the 8 lines it costs.
            video.currentTime = Math.max(0, video.currentTime + dir / 30);
            this.toast(dir > 0 ? 'frame →' : '← frame');
        },

        speed: function(video, delta) {
            const next = delta === 0 ? 1 : Math.min(8, Math.max(0.1, video.playbackRate + delta));
            video.playbackRate = next;
            this.toast(next.toFixed(2).replace(/\.?0+$/, '') + '×');
        },

        volume: function(video, delta) {
            // Past 1.0 the element itself clamps, so the extra range is the gain node's.
            const c = this.chains.get(video);
            const current = c ? c.gain.gain.value : 1;
            if (video.volume < 1 && delta < 0) {
                video.volume = Math.max(0, video.volume + delta);
            } else if (video.volume >= 1 && (delta > 0 || current > 1)) {
                const next = Math.min(4, Math.max(1, current + delta));
                if (this.setGain(video, next)) this.toast(Math.round(next * 100) + '%');
                return;
            } else {
                video.volume = Math.min(1, video.volume + delta);
            }
            this.toast(Math.round(video.volume * 100) + '%');
        },

        pip: function(video) {
            try {
                if (typeof video.requestPictureInPicture === 'function') {
                    if (document.pictureInPictureElement) document.exitPictureInPicture();
                    else video.requestPictureInPicture();
                } else if (typeof video.webkitSetPresentationMode === 'function') {
                    const mode = video.webkitPresentationMode === 'picture-in-picture' ? 'inline' : 'picture-in-picture';
                    video.webkitSetPresentationMode(mode);
                } else {
                    this.toast('No picture-in-picture on this video');
                }
            } catch (e) {
                this.toast('Picture-in-picture refused');
            }
        },

        // ---- subtitles ----
        // SubRip is the format subtitles are actually distributed in, and the only thing
        // standing between it and a <track> is the timestamp separator and a header.
        srtToVtt: function(text) {
            return 'WEBVTT\n\n' + text
                .replace(/\r\n/g, '\n')
                .replace(/(\d\d:\d\d:\d\d),(\d\d\d)/g, '$1.$2');
        },

        loadSubtitles: function(video, name, text) {
            const vtt = /^\s*WEBVTT/.test(text) ? text : this.srtToVtt(text);
            const url = URL.createObjectURL(new Blob([vtt], { type: 'text/vtt' }));
            Array.from(video.querySelectorAll('track[data-feather]')).forEach(t => t.remove());
            const track = document.createElement('track');
            track.setAttribute('data-feather', '1');
            track.kind = 'subtitles';
            track.label = name;
            track.srclang = 'und';
            track.src = url;
            track.default = true;
            video.appendChild(track);
            setTimeout(() => {
                for (const t of video.textTracks) {
                    t.mode = t.label === name ? 'showing' : 'disabled';
                }
            }, 60);
            this.toast('Subtitles: ' + name);
        },

        cueStyle: null,
        setCueStyle: function(size, color, bg) {
            if (!this.cueStyle) {
                this.cueStyle = document.createElement('style');
                (document.head || document.documentElement).appendChild(this.cueStyle);
            }
            this.cueStyle.textContent = '::cue, ::-webkit-media-text-track-display {'
                + ' font-size: ' + size + '% !important;'
                + ' color: ' + color + ' !important;'
                + ' background: ' + bg + ' !important; }';
        },

        // ---- panel ----
        toggle: function() {
            if (this.host) return this.close();
            const video = this.active();
            if (video) return this.open(video);
            if (this.broadcast({ cmd: 'toggle' })) return;
            // Only the top frame says so out loud. A toast inside a 1x1 tracking iframe
            // is noise nobody can see.
            if (this.isTop) this.toast('No video on this page');
        },

        close: function() {
            if (this.host && this.host.parentNode) this.host.parentNode.removeChild(this.host);
            this.host = null;
            this.panel = null;
        },

        open: function(video) {
            const host = document.createElement('div');
            const root = host.attachShadow({ mode: 'open' });
            const p = document.createElement('div');
            p.style.cssText = 'position: fixed; right: 16px; bottom: 16px; z-index: 2147483646; width: 260px; background: #14141c; color: #e6e6f0; border: 1px solid #2e2e42; border-radius: 10px; padding: 12px 14px; box-shadow: 0 16px 40px rgba(0,0,0,0.55); font: 13px -apple-system, system-ui, sans-serif; display: flex; flex-direction: column; gap: 10px; max-height: 80vh; overflow-y: auto;';

            const self = this;
            const head = document.createElement('div');
            head.style.cssText = 'display: flex; justify-content: space-between; align-items: center; font-weight: 600;';
            head.innerHTML = '<span>Video</span>';
            const x = document.createElement('span');
            x.textContent = '×';
            x.style.cssText = 'cursor: pointer; color: #8a8aa5; font-size: 16px;';
            x.onclick = () => self.close();
            head.appendChild(x);
            p.appendChild(head);

            function row(label, node) {
                const r = document.createElement('label');
                r.style.cssText = 'display: flex; align-items: center; justify-content: space-between; gap: 10px; color: #b9b9cc;';
                const s = document.createElement('span');
                s.textContent = label;
                r.appendChild(s);
                r.appendChild(node);
                p.appendChild(r);
                return r;
            }

            function slider(min, max, step, value, oninput) {
                const i = document.createElement('input');
                i.type = 'range';
                i.min = min; i.max = max; i.step = step; i.value = value;
                i.style.cssText = 'width: 120px; accent-color: #6f8cff;';
                i.oninput = () => oninput(parseFloat(i.value));
                return i;
            }

            function section(title) {
                const h = document.createElement('div');
                h.textContent = title;
                h.style.cssText = 'font-size: 10px; letter-spacing: 0.09em; text-transform: uppercase; color: #6f6f88; margin-top: 4px;';
                p.appendChild(h);
            }

            section('Playback');
            const speedOut = document.createElement('span');
            speedOut.textContent = video.playbackRate.toFixed(2) + '×';
            speedOut.style.cssText = 'color: #e6e6f0; font-variant-numeric: tabular-nums; width: 46px; text-align: right;';
            const speedRow = row('Speed', slider(0.25, 4, 0.05, video.playbackRate, v => {
                video.playbackRate = v;
                speedOut.textContent = v.toFixed(2) + '×';
            }));
            speedRow.appendChild(speedOut);

            const loop = document.createElement('input');
            loop.type = 'checkbox';
            loop.checked = video.loop;
            loop.onchange = () => { video.loop = loop.checked; };
            row('Loop', loop);

            const pipBtn = document.createElement('button');
            pipBtn.textContent = 'Open';
            pipBtn.style.cssText = 'background: #262634; color: #e6e6f0; border: 1px solid #3a3a52; border-radius: 5px; padding: 3px 10px; cursor: pointer; font: inherit;';
            pipBtn.onclick = () => self.pip(video);
            row('Picture-in-picture', pipBtn);

            section('Audio');
            if (self.audioIsSafe(video)) {
                const volOut = document.createElement('span');
                volOut.style.cssText = 'color: #e6e6f0; font-variant-numeric: tabular-nums; width: 46px; text-align: right;';
                volOut.textContent = '100%';
                const volRow = row('Boost', slider(1, 4, 0.05, 1, v => {
                    self.setGain(video, v);
                    volOut.textContent = Math.round(v * 100) + '%';
                }));
                volRow.appendChild(volOut);

                ['Bass', 'Mid', 'Treble'].forEach((name, i) => {
                    row(name, slider(-15, 15, 1, 0, v => {
                        const c = self.chain(video);
                        if (c) c.bands[i].gain.value = v;
                    }));
                });

                const comp = document.createElement('input');
                comp.type = 'checkbox';
                comp.onchange = () => {
                    const c = self.chain(video);
                    // Threshold at 0 dB is a compressor that never engages, which is how
                    // "off" is expressed without rebuilding the graph.
                    if (c) c.comp.threshold.value = comp.checked ? -30 : 0;
                };
                row('Compressor', comp);

                const mono = document.createElement('input');
                mono.type = 'checkbox';
                mono.onchange = () => self.setMono(video, mono.checked);
                row('Mono', mono);
            } else {
                const note = document.createElement('div');
                note.textContent = 'Cross-origin media — Web Audio would silence it, so boost and EQ are off here.';
                note.style.cssText = 'color: #8a8aa5; font-size: 11px; line-height: 1.4;';
                p.appendChild(note);
            }

            section('Picture');
            [['Brightness', 'brightness'], ['Contrast', 'contrast'], ['Saturation', 'saturate']].forEach(([label, key]) => {
                row(label, slider(0.2, 2, 0.05, self.picture[key], v => {
                    self.picture[key] = v;
                    self.applyPicture(video);
                }));
            });
            const gray = document.createElement('input');
            gray.type = 'checkbox';
            gray.onchange = () => { self.picture.grayscale = gray.checked ? 1 : 0; self.applyPicture(video); };
            row('Grayscale', gray);

            section('Buffer');
            const bt = document.createElement('input');
            bt.type = 'checkbox';
            bt.checked = PlayerTune.on;
            const btNote = document.createElement('div');
            btNote.style.cssText = 'color: #8a8aa5; font-size: 11px; line-height: 1.4;';
            btNote.textContent = PlayerTune.on
                ? 'Holding up to 180s ahead.'
                : 'Asks the site\u2019s own player to hold 180s ahead instead of the 30s it ships with. Works even where the playlist is closed to scripts.';
            bt.onchange = function() {
                if (!bt.checked) {
                    PlayerTune.disable();
                    btNote.textContent = 'Off \u2014 back to the player\u2019s own setting.';
                    return;
                }
                const r = PlayerTune.enable(video);
                if (!r) {
                    bt.checked = false;
                    btNote.textContent = 'This player keeps no buffer setting a script can reach \u2014 the browser decides, and it decides small.';
                } else if (r.kind === 'later') {
                    btNote.textContent = 'Set for the next time the player starts \u2014 change quality or reload to pick it up.';
                } else {
                    btNote.textContent = 'On \u2014 ' + r.kind + ', ' + (r.was || 30) + 's \u2192 180s ahead.';
                }
            };
            row('Deep buffer', bt);
            p.appendChild(btNote);

            section('Prebuffer');
            const pb = document.createElement('input');
            pb.type = 'checkbox';
            pb.checked = Prebuffer.on;
            const pbNote = document.createElement('div');
            pbNote.style.cssText = 'color: #8a8aa5; font-size: 11px; line-height: 1.4;';
            pbNote.textContent = Prebuffer.on
                ? 'Fetching ahead, 6 segments at a time.'
                : 'Fetches the next two minutes of segments in parallel so the player stops waiting on each one. Uses bandwidth ahead of playback.';
            pb.onchange = function() {
                if (!pb.checked) {
                    Prebuffer.disable();
                    pbNote.textContent = 'Off.';
                    return;
                }
                pbNote.textContent = 'Reading the playlist\u2026';
                Prebuffer.enable(video, function(err, count) {
                    if (err) {
                        pb.checked = false;
                        pbNote.textContent = err;
                    } else {
                        pbNote.textContent = 'On \u2014 ' + count + ' segments, 6 at a time.';
                    }
                });
            };
            row('Fetch ahead', pb);
            p.appendChild(pbNote);

            section('Download');
            const src = video.currentSrc || '';
            const playlist = /\.(m3u8|mpd)(\?|$)/i.test(src);
            const direct = /^https?:/.test(src) && !playlist;
            if (direct) {
                const dl = document.createElement('button');
                dl.textContent = 'Download';
                dl.style.cssText = 'background: #2c3d6b; color: #dce6ff; border: 1px solid #3d5183; border-radius: 5px; padding: 3px 10px; cursor: pointer; font: inherit;';
                dl.onclick = function() {
                    VideoLayer.requestDownload(video.currentSrc);
                    dl.textContent = 'Sent to downloads';
                    dl.disabled = true;
                };
                row('This video', dl);
            } else if (playlist) {
                // Whether the stream can be saved is the site's decision, not a list kept
                // here: if the playlist is readable by page script, it is readable. A site
                // that blocks script access to it has said no, and that answer stands.
                const note = document.createElement('div');
                note.textContent = 'Checking whether this stream is readable\u2026';
                note.style.cssText = 'color: #8a8aa5; font-size: 11px; line-height: 1.4;';
                p.appendChild(note);
                fetch(src, { method: 'GET', credentials: 'include' }).then(function(r) {
                    if (!r.ok) throw new Error(String(r.status));
                    note.remove();
                    const dl = document.createElement('button');
                    dl.textContent = 'Download stream';
                    dl.style.cssText = 'background: #2c3d6b; color: #dce6ff; border: 1px solid #3d5183; border-radius: 5px; padding: 3px 10px; cursor: pointer; font: inherit;';
                    dl.onclick = function() {
                        VideoLayer.requestDownload(src);
                        dl.textContent = 'Sent to downloads';
                        dl.disabled = true;
                    };
                    row('Segmented stream', dl);
                }).catch(function() {
                    note.textContent = 'This site does not allow scripts to read its stream, '
                        + 'so Feather cannot assemble it.';
                });
            } else {
                const note = document.createElement('div');
                note.textContent = 'This player feeds the video through Media Source Extensions, so there is no single file URL to fetch. Use the site\u2019s own download if it has one.';
                note.style.cssText = 'color: #8a8aa5; font-size: 11px; line-height: 1.4;';
                p.appendChild(note);
            }

            section('Subtitles');
            const file = document.createElement('input');
            file.type = 'file';
            file.accept = '.vtt,.srt,text/vtt';
            file.style.cssText = 'width: 130px; font-size: 11px; color: #b9b9cc;';
            file.onchange = () => {
                const f = file.files && file.files[0];
                if (!f) return;
                const reader = new FileReader();
                reader.onload = () => self.loadSubtitles(video, f.name, String(reader.result));
                reader.readAsText(f);
            };
            row('Load .srt / .vtt', file);

            let cueSize = 100, cueColor = '#ffffff', cueBg = 'rgba(0,0,0,0.6)';
            row('Caption size', slider(60, 260, 10, cueSize, v => {
                cueSize = v;
                self.setCueStyle(cueSize, cueColor, cueBg);
            }));
            const color = document.createElement('input');
            color.type = 'color';
            color.value = cueColor;
            color.style.cssText = 'width: 40px; height: 22px; background: none; border: 1px solid #3a3a52; border-radius: 4px;';
            color.oninput = () => { cueColor = color.value; self.setCueStyle(cueSize, cueColor, cueBg); };
            row('Caption colour', color);

            root.appendChild(p);
            (document.body || document.documentElement).appendChild(host);
            this.host = host;
            this.panel = p;
        },

        // Alt-based, because every other combination is spoken for: bare letters belong to
        // the page, Cmd to the browser, and a site that binds Alt+ArrowLeft is rare.
        handleKey: function(e) {
            if (!e.altKey || e.metaKey || e.ctrlKey) return false;
            if (!this.applyKey(e.key)) return false;
            e.preventDefault();
            return true;
        },

        // Separate from the event so a key forwarded from the parent frame takes the same
        // path as one typed here.
        applyKey: function(key) {
            if (key === 'v' || key === '\u221a') {
                this.toggle();
                return true;
            }
            if (key === 's' || key === '\u00df') {
                StreamPanel.toggle();
                return true;
            }
            const video = this.active();
            if (!video) return this.broadcast({ cmd: 'key', key: key });

            switch (key) {
                case 'ArrowLeft': this.seek(video, -5); break;
                case 'ArrowRight': this.seek(video, 5); break;
                case 'ArrowUp': this.volume(video, 0.1); break;
                case 'ArrowDown': this.volume(video, -0.1); break;
                case '[': case '\u201c': this.speed(video, -0.25); break;
                case ']': case '\u2018': this.speed(video, 0.25); break;
                case '\\': case '\u00ab': this.speed(video, 0); break;
                case ',': case '\u2264': this.frameStep(video, -1); break;
                case '.': case '\u2265': this.frameStep(video, 1); break;
                case 'p': case '\u03c0': this.pip(video); break;
                case 'l': case '\u00ac': video.loop = !video.loop; this.toast(video.loop ? 'Loop on' : 'Loop off'); break;
                case 'm': case '\u00b5': video.muted = !video.muted; this.toast(video.muted ? 'Muted' : 'Unmuted'); break;
                default: return false;
            }
            return true;
        }
    };

    // ---- streaming telemetry ----
    // What a player knows about its own health but never shows you: how many seconds of
    // video are actually in hand, how often it ran dry, whether the adaptive bitrate
    // ladder is climbing or falling, and what the network is doing to cause it.
    const StreamStats = {
        history: [],        // one sample per second, newest last
        rebuffers: 0,
        lastRes: '',
        resChanges: 0,
        timer: null,
        maxSamples: 120,

        bufferAhead: function(v) {
            const t = v.currentTime;
            for (let i = 0; i < v.buffered.length; i++) {
                if (v.buffered.start(i) <= t + 0.1 && v.buffered.end(i) >= t) {
                    return v.buffered.end(i) - t;
                }
            }
            return 0;
        },

        ranges: function(v) {
            const out = [];
            for (let i = 0; i < v.buffered.length; i++) {
                out.push([v.buffered.start(i), v.buffered.end(i)]);
            }
            return out;
        },

        // Resource Timing reports cross-origin requests too. Sizes are zeroed without
        // Timing-Allow-Origin, so bytes are best-effort and concurrency is not.
        network: function(since) {
            let bytes = 0, count = 0, inflight = 0;
            const now = performance.now();
            let entries = [];
            try { entries = performance.getEntriesByType('resource'); } catch (e) { return null; }
            for (const e of entries) {
                if (e.startTime < since) continue;
                count++;
                bytes += e.transferSize || e.encodedBodySize || 0;
                if (e.responseEnd === 0 || e.responseEnd > now - 50) inflight++;
            }
            return { bytes: bytes, count: count, inflight: inflight };
        },

        sample: function() {
            const v = VideoLayer.active();
            if (!v) return;
            const last = this.history[this.history.length - 1];
            const since = last ? last.at : performance.now() - 1000;
            const net = this.network(since) || { bytes: 0, count: 0, inflight: 0 };

            let dropped = 0, total = 0;
            if (typeof v.getVideoPlaybackQuality === 'function') {
                const q = v.getVideoPlaybackQuality();
                dropped = q.droppedVideoFrames || 0;
                total = q.totalVideoFrames || 0;
            }
            const res = v.videoWidth + 'x' + v.videoHeight;
            if (res !== this.lastRes && v.videoWidth) {
                if (this.lastRes) this.resChanges++;
                this.lastRes = res;
            }
            this.history.push({
                at: performance.now(),
                ahead: this.bufferAhead(v),
                bytes: net.bytes,
                reqs: net.count,
                inflight: net.inflight,
                dropped: dropped,
                total: total,
                res: res,
                stalled: v.readyState < 3 && !v.paused
            });
            if (this.history.length > this.maxSamples) this.history.shift();
        },

        start: function() {
            if (this.timer) return;
            const self = this;
            const v = VideoLayer.active();
            if (v) {
                v.addEventListener('waiting', function() { self.rebuffers++; });
            }
            this.timer = setInterval(function() { self.sample(); }, 1000);
            this.sample();
        },

        stop: function() {
            clearInterval(this.timer);
            this.timer = null;
        },

        summary: function() {
            const h = this.history;
            const last = h[h.length - 1] || {};
            // Throughput over the last ten samples rather than the last one: a single
            // second is mostly noise on a segmented stream that fetches in bursts.
            const window = h.slice(-10);
            const bytes = window.reduce(function(a, s) { return a + s.bytes; }, 0);
            const secs = window.length || 1;
            return {
                ahead: last.ahead || 0,
                kbps: (bytes * 8) / secs / 1000,
                res: last.res || '—',
                dropped: last.dropped || 0,
                total: last.total || 0,
                inflight: last.inflight || 0,
                rebuffers: this.rebuffers,
                resChanges: this.resChanges
            };
        }
    };

    // ---- parallel prebuffer ----
    // The sawtooth a segmented player draws — fetch one piece, drain it, fetch the next —
    // is not a bandwidth problem, it is a waiting problem: the next request does not start
    // until the last one finished. Fetching the upcoming segments ahead of time, several at
    // once, means they are already in the HTTP cache when the player asks.
    //
    // Deliberately NOT a replacement player. MSE cannot accept MPEG-TS, so feeding the
    // element directly would mean shipping a transmuxer, and taking over playback would
    // mean reimplementing the bitrate switching and error recovery the site already has.
    // Priming the cache leaves all of that alone and is about two hundred lines.
    const Prebuffer = {
        on: false,
        segments: [],        // { url, duration, start }
        primed: new Set(),
        inflight: 0,
        bytes: 0,
        hits: 0,
        failures: 0,
        timer: null,
        video: null,

        // How far ahead to run. Past this the player is comfortable and more fetching is
        // just someone else's bandwidth bill.
        AHEAD_SECONDS: 120,
        CONCURRENCY: 6,

        parse: function(text, base) {
            const lines = text.split('\n');
            const segs = [];
            let variant = null, bestBw = 0, pendingBw = null, duration = 0, start = 0;
            for (let raw of lines) {
                const line = raw.trim();
                if (!line) continue;
                if (line.indexOf('#EXT-X-STREAM-INF:') === 0) {
                    const m = /BANDWIDTH=(\d+)/.exec(line);
                    pendingBw = m ? parseInt(m[1], 10) : 0;
                    continue;
                }
                if (line.indexOf('#EXTINF:') === 0) {
                    duration = parseFloat(line.slice(8)) || 0;
                    continue;
                }
                if (line.charAt(0) === '#') continue;
                if (pendingBw !== null) {
                    if (pendingBw >= bestBw) { bestBw = pendingBw; variant = abs(base, line); }
                    pendingBw = null;
                    continue;
                }
                segs.push({ url: abs(base, line), duration: duration, start: start });
                start += duration;
            }
            return { segments: segs, variant: variant };
        },

        enable: function(video, done) {
            const src = video.currentSrc || video.src || '';
            const self = this;
            if (!/\.m3u8(\?|$)/i.test(src)) {
                return done('Prebuffering needs an HLS playlist; this player does not expose one.');
            }
            fetch(src, { credentials: 'include' }).then(function(r) {
                if (!r.ok) throw new Error(r.status);
                return r.text();
            }).then(function(text) {
                let list = self.parse(text, src);
                if (!list.segments.length && list.variant) {
                    return fetch(list.variant, { credentials: 'include' })
                        .then(function(r) { return r.text(); })
                        .then(function(t) { return self.parse(t, list.variant); });
                }
                return list;
            }).then(function(list) {
                if (!list.segments.length) throw new Error('no segments');
                self.segments = list.segments;
                self.video = video;
                self.on = true;
                self.timer = setInterval(function() { self.tick(); }, 1000);
                self.tick();
                done(null, list.segments.length);
            }).catch(function() {
                done('This site does not allow scripts to read its playlist, so there is nothing to fetch ahead.');
            });
        },

        disable: function() {
            this.on = false;
            clearInterval(this.timer);
            this.timer = null;
        },

        indexAt: function(t) {
            for (let i = 0; i < this.segments.length; i++) {
                const s = this.segments[i];
                if (t < s.start + s.duration) return i;
            }
            return this.segments.length;
        },

        tick: function() {
            if (!this.on || !this.video) return;
            const now = this.video.currentTime;
            const from = this.indexAt(now);
            let ahead = 0;
            for (let i = from; i < this.segments.length; i++) {
                if (this.inflight >= this.CONCURRENCY) return;
                const seg = this.segments[i];
                ahead = seg.start + seg.duration - now;
                if (ahead > this.AHEAD_SECONDS) return;
                if (this.primed.has(seg.url)) continue;
                this.fetchOne(seg);
            }
        },

        fetchOne: function(seg) {
            const self = this;
            // Marked before the request, not after: two ticks must not race into fetching
            // the same segment twice.
            this.primed.add(seg.url);
            this.inflight++;
            const started = performance.now();
            fetch(seg.url, { credentials: 'include' }).then(function(r) {
                if (!r.ok) throw new Error(r.status);
                return r.arrayBuffer();
            }).then(function(buf) {
                // The bytes are not kept. The point was to put them in the HTTP cache, and
                // holding a film in memory is the thing this browser exists not to do.
                self.bytes += buf.byteLength;
                self.hits++;
                if (performance.now() - started < 30) self.note = 'cached';
            }).catch(function() {
                self.failures++;
                self.primed.delete(seg.url);
            }).then(function() {
                self.inflight--;
            });
        },

        status: function() {
            if (!this.on) return null;
            return {
                primed: this.hits,
                total: this.segments.length,
                mb: this.bytes / 1048576,
                inflight: this.inflight,
                failures: this.failures
            };
        }
    };

    function abs(base, uri) {
        try { return new URL(uri, base).href; } catch (e) { return uri; }
    }

    // ---- player retune ----
    // The site's own player decides how deep the buffer goes, and hls.js ships a
    // thirty-second default that streaming sites never raise. That config object is the
    // one buffer lever still reachable when the playlist is closed to page script:
    // nothing here reads the stream, it asks the player already playing it to hold more.
    const PlayerTune = {
        on: false,
        Hls: null,
        target: null,       // the live instance that was changed
        before: null,       // what it held before
        defaults: null,     // and what the constructor held

        HLS: {
            maxBufferLength: 180,
            maxMaxBufferLength: 600,
            maxBufferSize: 200 * 1000 * 1000,
            backBufferLength: 30,
            startFragPrefetch: true
        },

        // hls.js is nearly always loaded as a plain global script, so the assignment can
        // be watched for. The value passes straight through: this observes, it does not
        // wrap the constructor, which is the kind of surgery that breaks a page.
        watch: function() {
            const self = this;
            let held = window.Hls;
            if (held) { this.Hls = held; return; }
            try {
                Object.defineProperty(window, 'Hls', {
                    configurable: true,
                    get: function() { return held; },
                    set: function(v) { held = v; self.Hls = v; if (self.on) self.seed(); }
                });
            } catch (e) {}
        },

        // Instances built after this point read their config from here. That covers the
        // re-inits a streaming page does constantly: quality switch, error recovery,
        // next episode. Without it the setting lasts until the first hiccup.
        seed: function() {
            try {
                const d = this.Hls.DefaultConfig;
                if (!this.defaults) {
                    this.defaults = {};
                    for (const k in this.HLS) this.defaults[k] = d[k];
                }
                Object.assign(d, this.HLS);
            } catch (e) {}
        },

        unseed: function() {
            try { if (this.defaults) Object.assign(this.Hls.DefaultConfig, this.defaults); } catch (e) {}
        },

        isHls: function(o) {
            try {
                return !!o.config && typeof o.config.maxBufferLength === 'number'
                    && typeof o.config.maxMaxBufferLength === 'number';
            } catch (e) { return false; }
        },

        isDash: function(o) {
            try {
                return typeof o.updateSettings === 'function'
                    && typeof o.getBufferLength === 'function';
            } catch (e) { return false; }
        },

        // A player sits on a global or one level under one (jwplayer().hls, player.core).
        // Two levels is where the returns stop and the odds of tripping a getter with
        // side effects start, so the walk stops there.
        // A bundled player exposes nothing globally, but React leaves its fiber on the
        // DOM node, and a player built with hooks keeps its engine in a ref inside that
        // fiber. Walking up from the video element finds what a scan of window cannot.
        fromFiber: function(video) {
            let key = null;
            try {
                for (const k of Object.keys(video)) {
                    if (k.indexOf('__reactFiber$') === 0) { key = k; break; }
                }
            } catch (e) {}
            if (!key) return null;
            let fiber = video[key];
            for (let up = 0; fiber && up < 24; up++, fiber = fiber.return) {
                // A class component puts the engine on stateNode; hooks hang off
                // memoizedState as a linked list. Both are worth a look, neither is deep.
                let hit = this.sniff(fiber.stateNode);
                if (hit) return hit;
                let hook = fiber.memoizedState;
                for (let n = 0; hook && n < 48; n++, hook = hook.next) {
                    const st = hook.memoizedState;
                    if (!st || typeof st !== 'object') continue;
                    hit = this.sniff(st) || this.sniff(st.current);
                    if (hit) return hit;
                }
            }
            return null;
        },

        sniff: function(o) {
            if (!o || typeof o !== 'object' || o.nodeType) return null;
            if (this.isHls(o)) return { kind: 'hls', obj: o };
            if (this.isDash(o)) return { kind: 'dash', obj: o };
            // One level in: a wrapper usually holds the engine as a plain field.
            let keys = [];
            try { keys = Object.keys(o); } catch (e) { return null; }
            if (keys.length > 30) return null;
            for (const k of keys) {
                let v;
                try { v = o[k]; } catch (e) { continue; }
                if (!v || typeof v !== 'object' || v.nodeType) continue;
                if (this.isHls(v)) return { kind: 'hls', obj: v };
                if (this.isDash(v)) return { kind: 'dash', obj: v };
            }
            return null;
        },

        find: function(video) {
            const viaFiber = video ? this.fromFiber(video) : null;
            if (viaFiber) return viaFiber;
            const pool = [];
            const add = function(o) {
                if (!o || typeof o !== 'object' || o.nodeType || o === window) return;
                if (pool.indexOf(o) < 0) pool.push(o);
            };
            let keys = [];
            try { keys = Object.keys(window); } catch (e) {}
            for (const k of keys) {
                let v;
                try { v = window[k]; } catch (e) { continue; }
                add(v);
                if (!v || typeof v !== 'object' || v.nodeType) continue;
                let sub = [];
                try { sub = Object.keys(v); } catch (e) { continue; }
                if (sub.length > 40) continue;
                for (const k2 of sub) { try { add(v[k2]); } catch (e) {} }
            }
            for (const o of pool) if (this.isHls(o)) return { kind: 'hls', obj: o };
            for (const o of pool) if (this.isDash(o)) return { kind: 'dash', obj: o };
            return null;
        },

        enable: function(video) {
            video.preload = 'auto';
            if (this.Hls) this.seed();
            const hit = this.find(video);
            if (!hit) {
                // The constructor was seeded but no instance is reachable: the setting is
                // real, it just does not land until the player next builds one.
                this.on = !!this.Hls;
                return this.on ? { kind: 'later', was: 0 } : null;
            }
            this.on = true;
            this.target = hit;
            if (hit.kind === 'hls') {
                const c = hit.obj.config;
                const was = c.maxBufferLength;
                this.before = {};
                for (const k in this.HLS) this.before[k] = c[k];
                // hls.js reads these on every buffer-fill tick, so a live object takes
                // the change without a restart.
                Object.assign(c, this.HLS);
                return { kind: 'hls.js', was: was };
            }
            let was = 0;
            try {
                const b = hit.obj.getSettings().streaming.buffer;
                was = b.bufferTimeAtTopQuality;
                this.before = { streaming: { buffer: Object.assign({}, b) } };
            } catch (e) { this.before = null; }
            hit.obj.updateSettings({ streaming: { buffer: {
                bufferTimeAtTopQuality: 180,
                bufferTimeAtTopQualityLongForm: 180,
                stableBufferTime: 60,
                bufferToKeep: 30
            } } });
            return { kind: 'dash.js', was: was };
        },

        disable: function() {
            this.on = false;
            this.unseed();
            const t = this.target;
            this.target = null;
            if (!t || !this.before) return;
            try {
                if (t.kind === 'hls') Object.assign(t.obj.config, this.before);
                else t.obj.updateSettings(this.before);
            } catch (e) {}
            this.before = null;
        }
    };

    // ---- stream monitor panel ----
    const StreamPanel = {
        host: null, canvas: null, ctx: null, timer: null, video: null,

        toggle: function() {
            if (this.host) return this.close();
            const video = VideoLayer.active();
            if (!video) {
                if (VideoLayer.broadcast({ cmd: 'stream' })) return;
                if (VideoLayer.isTop) VideoLayer.toast('No video on this page');
                return;
            }
            this.open(video);
        },

        close: function() {
            if (this.host && this.host.parentNode) this.host.parentNode.removeChild(this.host);
            this.host = null;
            clearInterval(this.timer);
            this.timer = null;
        },

        open: function(video) {
            this.video = video;
            StreamStats.start();
            const host = document.createElement('div');
            const root = host.attachShadow({ mode: 'open' });
            const wrap = document.createElement('div');
            wrap.style.cssText = 'position: fixed; left: 16px; bottom: 16px; z-index: 2147483645; width: 440px; background: rgba(11,12,18,0.94); color: #dfe3ee; border: 1px solid #262a3a; border-radius: 10px; padding: 12px 14px 10px; box-shadow: 0 18px 44px rgba(0,0,0,0.6); font: 12px ui-monospace, SFMono-Regular, Menlo, monospace;';

            const head = document.createElement('div');
            head.style.cssText = 'display:flex; justify-content:space-between; align-items:baseline; margin-bottom:8px;';
            head.innerHTML = '<span style="font-weight:600; letter-spacing:.04em; font-size:11px; color:#7f88a3; text-transform:uppercase;">Stream health</span>';
            const x = document.createElement('span');
            x.textContent = '×';
            x.style.cssText = 'cursor:pointer; color:#6d7690;';
            x.onclick = () => this.close();
            head.appendChild(x);
            wrap.appendChild(head);

            const canvas = document.createElement('canvas');
            canvas.width = 824;
            canvas.height = 300;
            canvas.style.cssText = 'width:412px; height:150px; display:block;';
            wrap.appendChild(canvas);

            const readout = document.createElement('div');
            readout.style.cssText = 'display:grid; grid-template-columns:repeat(4,1fr); gap:6px 10px; margin-top:9px;';
            wrap.appendChild(readout);

            root.appendChild(wrap);
            (document.body || document.documentElement).appendChild(host);
            this.host = host;
            this.canvas = canvas;
            this.ctx = canvas.getContext('2d');
            this.readout = readout;

            const self = this;
            this.timer = setInterval(function() { self.draw(); }, 500);
            this.draw();
        },

        cell: function(label, value, tone) {
            const d = document.createElement('div');
            const colors = { ok: '#6fd1a0', warn: '#e2b657', bad: '#e8737d', '': '#dfe3ee' };
            d.innerHTML = '<div style="color:#6d7690; font-size:9px; letter-spacing:.06em; text-transform:uppercase;">'
                + label + '</div><div style="color:' + (colors[tone || ''] || '#dfe3ee')
                + '; font-size:13px; margin-top:1px;">' + value + '</div>';
            return d;
        },

        draw: function() {
            const v = this.video;
            if (!v || !this.ctx) return;
            const s = StreamStats.summary();
            const c = this.ctx, W = this.canvas.width, H = this.canvas.height;
            const timelineH = 54, gap = 18;
            const graphTop = timelineH + gap;
            const graphH = H - graphTop - 6;

            c.clearRect(0, 0, W, H);

            // --- buffered-range timeline: what the player actually holds of the whole film
            const dur = isFinite(v.duration) && v.duration > 0 ? v.duration : 1;
            c.fillStyle = '#1b1f2c';
            c.fillRect(0, 16, W, timelineH - 26);
            for (const [a, b] of StreamStats.ranges(v)) {
                const x0 = (a / dur) * W, x1 = (b / dur) * W;
                c.fillStyle = '#3d6be0';
                c.fillRect(x0, 16, Math.max(1, x1 - x0), timelineH - 26);
            }
            const px = (v.currentTime / dur) * W;
            c.fillStyle = '#ffd479';
            c.fillRect(px - 1.5, 10, 3, timelineH - 14);

            c.fillStyle = '#6d7690';
            c.font = '18px ui-monospace, Menlo, monospace';
            c.fillText(fmtTime(v.currentTime), 0, 12);
            c.textAlign = 'right';
            c.fillText(fmtTime(dur), W, 12);
            c.textAlign = 'left';

            // --- buffer-ahead history: the line that dips before a stall
            const h = StreamStats.history;
            const maxAhead = Math.max(30, ...h.map(function(p) { return p.ahead; }));
            c.strokeStyle = '#262a3a';
            c.lineWidth = 1;
            [0.25, 0.5, 0.75].forEach(function(f) {
                const y = graphTop + graphH * f;
                c.beginPath(); c.moveTo(0, y); c.lineTo(W, y); c.stroke();
            });

            if (h.length > 1) {
                const step = W / (StreamStats.maxSamples - 1);
                // area under the buffer line
                c.beginPath();
                c.moveTo(0, graphTop + graphH);
                h.forEach(function(p, i) {
                    c.lineTo(i * step, graphTop + graphH - (p.ahead / maxAhead) * graphH);
                });
                c.lineTo((h.length - 1) * step, graphTop + graphH);
                c.closePath();
                c.fillStyle = 'rgba(61,107,224,0.18)';
                c.fill();

                c.beginPath();
                h.forEach(function(p, i) {
                    const y = graphTop + graphH - (p.ahead / maxAhead) * graphH;
                    if (i === 0) c.moveTo(0, y); else c.lineTo(i * step, y);
                });
                c.strokeStyle = '#5b8cff';
                c.lineWidth = 2;
                c.stroke();

                // a request that landed in that second, drawn as a tick along the floor
                h.forEach(function(p, i) {
                    if (!p.reqs) return;
                    const x = i * step;
                    c.fillStyle = 'rgba(111,209,160,' + Math.min(1, 0.25 + p.reqs / 8) + ')';
                    c.fillRect(x - 1, graphTop + graphH - 4, 2, 4);
                });

                // seconds where the player had nothing to show
                h.forEach(function(p, i) {
                    if (!p.stalled) return;
                    c.fillStyle = 'rgba(232,115,125,0.35)';
                    c.fillRect(i * step - step / 2, graphTop, step, graphH);
                });
            }

            c.fillStyle = '#6d7690';
            c.font = '17px ui-monospace, Menlo, monospace';
            c.fillText(Math.round(maxAhead) + 's buffered', 4, graphTop + 16);
            c.fillText('2 min', 4, graphTop + graphH - 6);

            // --- readout
            const tone = s.ahead > 20 ? 'ok' : (s.ahead > 5 ? 'warn' : 'bad');
            const dropPct = s.total ? (s.dropped / s.total) * 100 : 0;
            this.readout.textContent = '';
            this.readout.appendChild(this.cell('Buffer', s.ahead.toFixed(1) + 's', tone));
            this.readout.appendChild(this.cell('Throughput', s.kbps > 0 ? fmtRate(s.kbps) : 'n/a', ''));
            this.readout.appendChild(this.cell('Resolution', s.res, ''));
            this.readout.appendChild(this.cell('In flight', String(s.inflight), ''));
            this.readout.appendChild(this.cell('Rebuffers', String(s.rebuffers), s.rebuffers ? 'warn' : 'ok'));
            this.readout.appendChild(this.cell('Dropped', dropPct.toFixed(1) + '%', dropPct > 2 ? 'bad' : 'ok'));
            this.readout.appendChild(this.cell('ABR shifts', String(s.resChanges), ''));
            this.readout.appendChild(this.cell('Rate', v.playbackRate.toFixed(2) + '×', ''));

            const pb = Prebuffer.status();
            if (pb) {
                this.readout.appendChild(this.cell('Prefetched', pb.primed + '/' + pb.total, 'ok'));
                this.readout.appendChild(this.cell('Ahead of play', pb.mb.toFixed(1) + ' MB', ''));
                this.readout.appendChild(this.cell('Fetching', String(pb.inflight), ''));
                this.readout.appendChild(this.cell('Misses', String(pb.failures), pb.failures ? 'warn' : 'ok'));
            }
        }
    };

    function fmtTime(t) {
        if (!isFinite(t)) return '--:--';
        const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = Math.floor(t % 60);
        const pad = function(n) { return n < 10 ? '0' + n : String(n); };
        return (h ? h + ':' + pad(m) : String(m)) + ':' + pad(s);
    }

    function fmtRate(kbps) {
        return kbps >= 1000 ? (kbps / 1000).toFixed(1) + ' Mbps' : Math.round(kbps) + ' kbps';
    }

    PlayerTune.watch();

    VideoLayer.tune = PlayerTune;
    VideoLayer.stream = StreamPanel;
    VideoLayer.prebuffer = Prebuffer;
    window.__FeatherVideo = VideoLayer;

    // Commands arrive from the embedding frame and nowhere else. A page can post to its
    // own iframes, but the worst that buys it is opening a panel over its own video.
    window.addEventListener('message', function(e) {
        const d = e.data;
        if (!d || d.__feather !== 'video') return;

        // State flows up from a child; commands flow down from the embedder. Anything
        // else is a page talking to itself, and is ignored.
        if (d.cmd === 'download') {
            if (e.source === window.parent) return;
            if (VideoLayer.isTop) {
                try {
                    window.ipc.postMessage(JSON.stringify({
                        type: 'download_media', url: d.key, referer: d.referer
                    }));
                } catch (err) {}
            } else {
                try { window.parent.postMessage(d, '*'); } catch (err) {}
            }
            return;
        }
        if (d.cmd === 'state') {
            if (e.source === window.parent) return;
            VideoLayer.childState.set(e.source, {
                playing: !!d.playing, muted: !!d.muted,
                position: d.position, duration: d.duration
            });
            VideoLayer.report();
            return;
        }
        if (e.source !== window.parent) return;
        if (d.cmd === 'toggle') VideoLayer.toggle();
        else if (d.cmd === 'key') VideoLayer.applyKey(d.key);
        else if (d.cmd === 'mute') VideoLayer.setMuted(!!d.on);
        else if (d.cmd === 'stream') StreamPanel.toggle();
        else if (d.cmd === 'resume') VideoLayer.resumeAt(d.key);
    });

    VideoLayer.watch();

    // Position is reported on a timer rather than on timeupdate, which fires four times a
    // second and would cost a database write each time.
    setInterval(function() {
        const p = VideoLayer.progress();
        if (!p) return;
        if (VideoLayer.isTop) {
            try {
                window.ipc.postMessage(JSON.stringify({
                    type: 'media_progress', position: p.position, duration: p.duration
                }));
            } catch (e) {}
        } else {
            try {
                window.parent.postMessage({
                    __feather: 'video', cmd: 'state', playing: true,
                    muted: VideoLayer.lastSent ? VideoLayer.lastSent.muted : false,
                    position: p.position, duration: p.duration
                }, '*');
            } catch (e) {}
        }
    }, 10000);

    // The top frame routes Alt keys through runtime.js, which orders them against the
    // find bar and the vim layer. A subframe has none of that, so it listens directly.
    if (window.top !== window) {
        window.addEventListener('keydown', function(e) {
            if (e.altKey && !e.metaKey && !e.ctrlKey) VideoLayer.handleKey(e);
        }, true);
    }
})();
