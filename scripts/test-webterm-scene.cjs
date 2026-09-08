const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function fixture() {
  let now = 1000, timerId = 0, blobId = 0;
  const timers = new Map(), listeners = {}, requests = [], revoked = [];
  const metrics = { w: 8, h: 16 };
  class Element {
    constructor(tag) { this.tagName = tag; this.children = []; this.style = {}; this.dataset = {}; this.attributes = {}; this.hidden = false; }
    appendChild(node) { node.remove(); this.children.push(node); node.parent = this; return node; }
    append(...nodes) { nodes.forEach(node => this.appendChild(node)); }
    replaceChildren(...nodes) { this.children.forEach(node => { node.parent = null; }); this.children = []; this.append(...nodes); }
    remove() { if (this.parent) this.parent.children = this.parent.children.filter(node => node !== this); this.parent = null; }
    setAttribute(key, value) { this.attributes[key] = value; }
    cloneNode() { const copy = new Element(this.tagName); copy.className = this.className; copy.textContent = this.textContent; return copy; }
    getBoundingClientRect() { return { width: metrics.w, height: metrics.h }; }
  }
  const reduced = { matches: false, addEventListener(name, callback) { this.change = callback; } };
  const document = {
    visibilityState: 'visible', body: new Element('body'), createElement: tag => new Element(tag),
    addEventListener(name, callback) { (listeners[name] ||= []).push(callback); },
  };
  const window = { matchMedia: () => reduced, addEventListener(name, callback) { (listeners[name] ||= []).push(callback); } };
  const context = {
    window, document, AbortController, Set, Map, Date: { now: () => now },
    CSS: { supports: () => true }, getComputedStyle: () => ({ fontFamily: 'mono', fontSize: '13px', lineHeight: '16px' }),
    URL: { createObjectURL: () => `blob:fixture-${++blobId}`, revokeObjectURL: url => revoked.push(url) },
    setTimeout(callback, delay) { const id = ++timerId; timers.set(id, { callback, delay }); return id; },
    clearTimeout: id => timers.delete(id),
    fetch: (url, options) => new Promise(resolve => requests.push({ url, options, resolve })),
  };
  vm.runInNewContext(fs.readFileSync('crates/kasa-mcp/assets/term/grid.js', 'utf8'), context);
  const root = new Element('main'), grid = window.KasaGrid(root, { assetRoot: '/u/fixture/m/device' });
  grid.setPane('%1');
  function find(className, parent = grid.el) {
    if (parent.className?.split(' ').includes(className)) return parent;
    for (const child of parent.children) { const result = find(className, child); if (result) return result; }
    return null;
  }
  return { grid, metrics, document, reduced, timers, requests, revoked, find,
    setNow: value => { now = value; },
    event: name => (listeners[name] || []).forEach(callback => callback()),
    respond(index, ok = true) { requests[index].resolve({ ok, blob: async () => ({ type: 'image/png', size: 100 }) }); },
    tick() { const entry = [...timers].find(([, timer]) => timer.delay < 8000); assert.ok(entry); timers.delete(entry[0]); entry[1].callback(); },
  };
}
const keyA = 'a'.repeat(64), keyB = 'b'.repeat(64), keyC = 'c'.repeat(64);
function raw(text = 'raw logo') {
  return { t: 'grid', cols: 12, rows: 2, dirty: [[0, [[text, null, null, 0]]], [1, [['prompt', null, null, 0]]]], cursor: [1, 0], cursorVisible: true };
}
function packet(revision = 1, key = keyA, asset = { kind: 'inline', id: keyA }) {
  return { ...raw('native cells'), sourceKey: key, sceneRevision: revision,
    scene: { paneId: '%1', sourceKey: key, revision, cols: 12, rows: 2, offset: 0,
      overlays: [{ id: 'banner', rect: { x: 2, y: 0, width: 4, height: 2 },
        clip: { x: 1, y: 0, width: 6, height: 2 }, z: 2, fit: 'contain', anchor: 'bottom', asset, motion: null }] } };
}
const flush = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

(async () => {
  {
    const f = fixture();
    assert.equal(f.grid.apply(raw()), true);
    assert.equal(f.grid.apply(packet()), true);
    assert.equal(f.find('kg-row').children[0].textContent, 'native cells');
    assert.equal(f.requests[0].url, `/u/fixture/m/device/term/visual-asset?pane=%251&id=${keyA}`);
    f.respond(0); await flush();
    const image = f.find('kg-scene-content');
    assert.equal(image.hidden, false);
    assert.equal(image.style.objectFit, 'contain');
    assert.equal(image.style.objectPosition, '50% 100%');
    assert.equal(image.style.left, '8px');
    assert.equal(f.find('kg-scene-item').style.width, '48px');
    f.metrics.w = 10; f.metrics.h = 20; f.grid.remeasure();
    assert.equal(image.style.left, '10px');
    assert.equal(image.style.height, '40px');
    const invalid = packet(2); invalid.sourceKey = keyB;
    assert.equal(f.grid.apply(invalid), false, 'mismatched cells/scene was accepted');
    assert.equal(f.grid.apply(packet(0)), false, 'old revision was accepted');
    const forged = packet(3); forged.scene.overlays[0].asset = { kind: 'inline', id: '../../private/file' };
    assert.equal(f.grid.apply(forged), false, 'asset path injection reached fetch');
    assert.equal(f.requests.length, 1);
    assert.equal(f.grid.apply({ ...raw('fresh raw'), scene: null }), true);
    assert.equal(f.grid.hasScene, false);
    assert.equal(f.find('kg-scene-front').children.length, 0);
    assert.equal(f.find('kg-row').children[0].textContent, 'fresh raw');
    f.grid.resetScene();
    assert.equal(f.revoked.length, 1);
  }
  {
    const f = fixture();
    f.grid.apply(packet());
    f.grid.apply(packet(2, keyB, { kind: 'inline', id: keyB }));
    f.respond(0); await flush();
    assert.equal(f.find('kg-scene-content').hidden, true, 'old image returned into the new scene');
    f.respond(1); await flush();
    assert.equal(f.find('kg-scene-content').hidden, false);
    f.grid.resetScene(); f.grid.setPane('%1');
    assert.equal(f.grid.apply(packet(0, keyC)), true, 'new server epoch could not reset revisions');
    f.respond(2, false); await flush();
    f.grid.apply(packet(0, keyC)); await flush();
    assert.equal(f.requests.length, 3, 'same revision retried a failed asset');
    f.grid.resetScene(); f.grid.setPane('%1'); f.grid.apply(packet(0, keyC));
    assert.equal(f.requests.length, 4, 'reconnect failed to retry the same asset hash');
    f.respond(3); await flush();
    assert.equal(f.find('kg-scene-content').hidden, false);
    f.grid.resetScene();
  }
  {
    const f = fixture();
    f.grid.apply(packet());
    f.respond(0, false); await flush();
    assert.equal(f.find('kg-scene-content').hidden, true);
    f.grid.apply(packet());
    assert.equal(f.requests.length, 1, 'same-scene repaint bypassed retry backoff');
    f.setNow(1500); f.tick();
    assert.equal(f.requests.length, 2, 'idle scene did not retry a temporary asset failure');
    f.respond(1); await flush();
    assert.equal(f.find('kg-scene-content').hidden, false, 'same revision stayed blank after asset recovery');
    assert.equal(f.grid.el.dataset.sceneRevision, '1');
    f.grid.resetScene();
  }
  {
    const f = fixture();
    f.grid.apply(packet());
    for (let attempt = 0; attempt < 5; attempt++) {
      f.respond(attempt, false); await flush();
      if (attempt < 4) { f.setNow(1000 + (attempt + 1) * 10000); f.tick(); }
    }
    assert.equal(f.requests.length, 5);
    assert.equal(f.timers.size, 0, 'failed idle asset exceeded its retry budget');
    f.event('online');
    assert.equal(f.requests.length, 6, 'network return did not renew the retry budget');
    f.respond(5); await flush();
    assert.equal(f.find('kg-scene-content').hidden, false);
    f.grid.resetScene();
  }
  {
    const f = fixture();
    const animated = packet(1, keyA, { kind: 'animation', frames: [keyA, keyB] });
    animated.scene.overlays[0].motion = { frames: 2, frameMs: 20, startedAtMs: 1000, looping: true };
    f.grid.apply(animated);
    assert.equal(f.requests.length, 2, 'current and next frame should share the bounded cache');
    f.respond(0); f.respond(1); await flush();
    const first = f.find('kg-scene-content').src;
    f.setNow(1025); f.tick(); await flush();
    assert.notEqual(f.find('kg-scene-content').src, first);
    assert.equal(f.requests.length, 2, 'animation refetched cached frames');
    f.document.visibilityState = 'hidden'; f.event('visibilitychange');
    assert.equal(f.timers.size, 0, 'hidden animation did not stop');
    f.document.visibilityState = 'visible'; f.reduced.matches = true; f.reduced.change(); await flush();
    assert.equal(f.find('kg-scene-content').src, first, 'reduced motion did not choose the static frame');
    assert.equal(f.timers.size, 0);
    f.grid.resetScene();
  }
  console.log('webterm native scene packet, geometry, asset lifetime and motion checks passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
