// Checks the part of the video layer that cannot be checked by reading it: finding the
// page's player. A bundled player puts nothing on window, so the search walks React's
// fiber off the video element instead, and that walk is easy to get subtly wrong.
//
//   node scripts/check-video.js
'use strict';
const assert = require('assert');
const fs = require('fs');
const path = require('path');

function el() {
    const e = { style: {}, children: [], nodeType: 1 };
    e.appendChild = c => { e.children.push(c); return c; };
    e.removeChild = () => {};
    e.setAttribute = () => {};
    e.addEventListener = () => {};
    e.attachShadow = () => el();
    e.getContext = () => null;
    return e;
}

const doc = {
    nodeType: 9,
    body: el(),
    documentElement: el(),
    createElement: el,
    querySelector: () => null,
    querySelectorAll: () => [],
    addEventListener: () => {},
};

const win = {
    document: doc,
    addEventListener: () => {},
    location: { href: 'https://example.test/', hostname: 'example.test' },
    navigator: { userAgent: 'node' },
};
win.top = win.parent = win.self = win;
global.window = win;
global.document = doc;
// node 22 owns the global navigator; the layer only reaches it through window anyway.

eval(fs.readFileSync(path.join(__dirname, '..', 'src', 'scriptlets', 'video.js'), 'utf8'));
const Tune = win.__FeatherVideo.tune;

// hls.js ships 30 seconds and an unbounded back buffer. Those are the numbers to move.
const engine = (seconds) => ({
    config: {
        maxBufferLength: seconds,
        maxMaxBufferLength: 600,
        maxBufferSize: 60e6,
        backBufferLength: Infinity,
        startFragPrefetch: false,
    },
});

// React hangs the fiber off the DOM node; a hooks component keeps its engine in a ref
// somewhere down memoizedState's linked list, which is the shape being walked.
function videoWithFiber(held) {
    const v = el();
    const host = { stateNode: v, memoizedState: null, return: null };
    host.return = {
        stateNode: null,
        return: null,
        memoizedState: {
            memoizedState: 'unrelated hook',
            next: { memoizedState: { current: held }, next: null },
        },
    };
    v['__reactFiber$k9x'] = host;
    return v;
}

// 1. No player anywhere: say so rather than claiming a win.
assert.strictEqual(Tune.enable(el()), null, 'a bare video is not a player');

// 2. Engine held only in a hook ref — the webpack-bundled case.
let hls = engine(30);
let video = videoWithFiber(hls);
let r = Tune.enable(video);
assert.strictEqual(r.kind, 'hls.js');
assert.strictEqual(r.was, 30, 'reports what the player had before');
assert.strictEqual(hls.config.maxBufferLength, 180);
assert.strictEqual(hls.config.backBufferLength, 30, 'back buffer trimmed to free MSE quota');
assert.strictEqual(video.preload, 'auto');
Tune.disable();
assert.strictEqual(hls.config.maxBufferLength, 30, 'off puts every value back');
assert.strictEqual(hls.config.backBufferLength, Infinity);

// 3. Engine one level under a wrapper, which is how most players hold it.
hls = engine(45);
r = Tune.enable(videoWithFiber({ name: 'player', engine: hls }));
assert.strictEqual(r.was, 45);
assert.strictEqual(hls.config.maxBufferLength, 180);
Tune.disable();

// 4. The easy case: the player is a global.
hls = engine(20);
win.somePlayer = { hls: hls };
r = Tune.enable(el());
assert.strictEqual(r.kind, 'hls.js');
assert.strictEqual(hls.config.maxBufferLength, 180);
Tune.disable();
delete win.somePlayer;

// 5. dash.js keeps its buffer settings behind a getter and a setter instead.
const dash = {
    s: { streaming: { buffer: { bufferTimeAtTopQuality: 30, stableBufferTime: 12, bufferToKeep: 20 } } },
    getSettings() { return this.s; },
    getBufferLength() { return 10; },
    updateSettings(o) { Object.assign(this.s.streaming.buffer, o.streaming.buffer); },
};
win.dashPlayer = dash;
r = Tune.enable(el());
assert.strictEqual(r.kind, 'dash.js');
assert.strictEqual(dash.s.streaming.buffer.bufferTimeAtTopQuality, 180);
Tune.disable();
assert.strictEqual(dash.s.streaming.buffer.bufferTimeAtTopQuality, 30);
assert.strictEqual(dash.s.streaming.buffer.bufferToKeep, 20);
delete win.dashPlayer;

// 6. No instance to reach, but the library is loaded: seed the constructor so the next
//    one the player builds — after a quality switch or an error — starts deep.
win.Hls = { DefaultConfig: { maxBufferLength: 30, maxMaxBufferLength: 600, maxBufferSize: 60e6, backBufferLength: Infinity, startFragPrefetch: false } };
r = Tune.enable(el());
assert.strictEqual(r.kind, 'later');
assert.strictEqual(win.Hls.DefaultConfig.maxBufferLength, 180);
assert.strictEqual(win.Hls, win.Hls, 'the constructor is observed, never replaced');
Tune.disable();
assert.strictEqual(win.Hls.DefaultConfig.maxBufferLength, 30);

console.log('video layer: player search ok (6 checks)');
process.exit(0);
