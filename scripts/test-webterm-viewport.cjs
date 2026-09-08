const assert = require('node:assert/strict');
const { create } = require('../crates/kasa-mcp/assets/term/viewport.js');

function fixture() {
  let visible = true;
  let target = { cols: 120, rows: 40 };
  const messages = [];
  const socket = { readyState: 1, send: text => messages.push(JSON.parse(text)) };
  const controller = create({ measure: () => target, visible: () => visible });
  controller.bind(socket);
  return { controller, socket, messages,
    visible: value => { visible = value; controller.visibilityChanged(); },
    target: value => { target = value; },
    size: (capable = true, mirror = true) => controller.receive({ t: 'size', cols: 21, rows: 6, mirror,
      ...(capable ? { capabilities: { mirror_viewport: 1 } } : {}) }, socket),
    ack: granted => controller.receive({ t: 'viewport', granted }, socket),
  };
}

{
  const f = fixture();
  f.controller.sync();
  assert.deepEqual(f.messages, [], 'onopen/font sizing must wait for the first size handshake');
  f.size(); f.controller.sync();
  assert.deepEqual(f.messages, [{ t: 'viewport', op: 'acquire', cols: 120, rows: 40 }]);
  f.target({ cols: 43, rows: 71 });
  f.controller.sync();
  assert.deepEqual(f.messages.at(-1), { t: 'viewport', op: 'resize', cols: 43, rows: 71 });
  f.ack(true); f.controller.sync();
  assert.equal(f.messages.length, 2, 'acknowledgement must not repeat the last viewport');
  f.ack(false);
  f.controller.setWanted(true);
  f.target({ cols: 80, rows: 15 });
  f.controller.sync();
  assert.deepEqual(f.messages.at(-1), { t: 'viewport', op: 'resize', cols: 80, rows: 15 });
  f.ack(false); f.controller.sync();
  assert.equal(f.messages.length, 3, 'a denied target must not enter a retry loop');
  f.target({ cols: 90, rows: 20 }); f.controller.sync();
  assert.deepEqual(f.messages.at(-1), { t: 'viewport', op: 'resize', cols: 90, rows: 20 },
    'restored owners must be able to resize without another acquire');
  f.controller.acquire(); f.controller.sync();
  assert.equal(f.messages.at(-1).op, 'acquire', 'an explicit fit action may request control');
  f.controller.release();
}

{
  const f = fixture();
  f.size(); f.controller.sync(); f.ack(true);
  f.visible(false);
  assert.deepEqual(f.messages.at(-1), { t: 'viewport', op: 'release' });
  const count = f.messages.length;
  f.controller.sync(); f.visible(false);
  assert.equal(f.messages.length, count, 'hidden pages must neither repeat release nor resize');
  f.target({ cols: 51, rows: 20 });
  f.visible(true); f.controller.sync();
  assert.deepEqual(f.messages.at(-1), { t: 'viewport', op: 'acquire', cols: 51, rows: 20 });
  const nextMessages = [];
  const next = { readyState: 1, send: text => nextMessages.push(JSON.parse(text)) };
  f.controller.bind(next);
  assert.equal(f.controller.receive({ t: 'viewport', granted: true }, f.socket), false);
  f.controller.disconnected(f.socket);
  f.controller.receive({ t: 'size', cols: 21, rows: 6, mirror: true }, next);
  f.controller.sync();
  assert.deepEqual(nextMessages, [], 'a reconnected old host must receive no automatic force/viewport');
  assert.equal(f.controller.state().capable, false);
  f.controller.release();
}

{
  const f = fixture();
  f.size(false, false); f.controller.sync();
  assert.deepEqual(f.messages, [{ t: 'resize', cols: 120, rows: 40 }]);
  f.controller.sync();
  assert.equal(f.messages.length, 1, 'identical native cell measurements must deduplicate');
  f.target({ cols: 43, rows: 20 }); f.controller.sync();
  assert.deepEqual(f.messages.at(-1), { t: 'resize', cols: 43, rows: 20 });
  f.target({ cols: 0, rows: 0 }); f.controller.sync();
  assert.equal(f.messages.length, 2, 'a temporarily hidden measurement must not shrink the PTY');
  f.controller.release();
}

console.log('webterm viewport controller lifecycle checks passed');
