(function (global) {
  'use strict';

  function createViewport(options) {
    let socket = null;
    let ready = false, mirror = false, capable = false;
    let requested = false, granted = false, wanted = true;
    let last = '', timer = null;
    const visible = options.visible || (() => document.visibilityState === 'visible');
    const state = () => ({ ready, mirror, capable, requested, granted, wanted });
    const changed = () => options.onState?.(state());
    const current = (candidate) => candidate === socket;
    const open = () => socket && socket.readyState === 1;
    function send(message) {
      if (!open()) return false;
      socket.send(JSON.stringify(message));
      return true;
    }
    function cancel() { clearTimeout(timer); timer = null; }
    function release() {
      cancel();
      if (ready && mirror && capable && requested) send({ t: 'viewport', op: 'release' });
      requested = false; granted = false; last = '';
      changed();
    }
    function sync() {
      cancel();
      // onopen can precede size: an unknown mirror must never receive legacy resize.
      if (!open() || !ready || !visible() || !wanted) return;
      if (mirror && !capable) return;
      const target = options.measure();
      if (!target || !Number.isFinite(target.cols) || !Number.isFinite(target.rows)
          || target.cols <= 0 || target.rows <= 0) return;
      const cols = Math.min(1000, Math.max(mirror ? 2 : 20, Math.floor(target.cols)));
      const rows = Math.min(1000, Math.max(mirror ? 1 : 5, Math.floor(target.rows)));
      const key = `${cols}:${rows}`;
      if (last === key) return;
      if (mirror) {
        const op = requested ? 'resize' : 'acquire';
        send({ t: 'viewport', op, cols, rows });
        requested = true;
      } else send({ t: 'resize', cols, rows });
      last = key;
      changed();
    }
    function schedule() {
      if (timer !== null) return;
      timer = setTimeout(sync, 60);
    }
    function bind(next) {
      release();
      socket = next;
      ready = false; mirror = false; capable = false;
      requested = false; granted = false; last = '';
      changed();
    }
    function receive(message, candidate) {
      if (!current(candidate)) return false;
      if (message.t === 'size') {
        if (!ready) {
          mirror = !!message.mirror;
          capable = message.capabilities?.mirror_viewport === 1;
          ready = true;
        }
        if (message.owner === false) granted = false;
        changed();
        schedule();
      } else if (message.t === 'viewport' && capable && mirror) {
        granted = message.granted === true && message.owner !== false;
        changed();
        // A newer viewer may own the PTY; resize is not permission to steal it back.
        if (granted) schedule();
      }
      return true;
    }
    function disconnected(candidate) {
      if (!current(candidate)) return;
      cancel(); socket = null;
      ready = false; mirror = false; capable = false;
      requested = false; granted = false; last = '';
      changed();
    }
    function setWanted(value) {
      value = !!value;
      if (wanted === value) return;
      wanted = value;
      if (!wanted) release();
      else { requested = false; granted = false; last = ''; schedule(); }
      changed();
    }
    function acquire() {
      wanted = true; requested = false; granted = false; last = '';
      schedule(); changed();
    }
    function visibilityChanged() {
      if (visible()) schedule();
      else release();
    }
    return { bind, receive, disconnected, current, sync, schedule, release,
      setWanted, acquire, visibilityChanged, state };
  }

  function visibleArea(element) {
    const rect = element.getBoundingClientRect();
    const viewport = global.visualViewport;
    const left = viewport?.offsetLeft || 0, top = viewport?.offsetTop || 0;
    const right = left + (viewport?.width || document.documentElement.clientWidth);
    const bottom = top + (viewport?.height || document.documentElement.clientHeight);
    return {
      width: Math.max(0, Math.min(rect.right, right) - Math.max(rect.left, left)),
      height: Math.max(0, Math.min(rect.bottom, bottom) - Math.max(rect.top, top)),
    };
  }

  const api = { create: createViewport, visibleArea };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else global.KasaViewport = api;
})(typeof window === 'undefined' ? globalThis : window);
