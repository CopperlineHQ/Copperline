// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { Miniflare } from 'miniflare';

const origin = 'https://copperline.dev';
const guest = 'g'.repeat(22);
const code = type => 'CLNP1.' + btoa(JSON.stringify({ description: { type, sdp: 'v=0\r\ns=-\r\n' } }));
const options = {
  modules: true, scriptPath: new URL('./worker.js', import.meta.url).pathname,
  compatibilityDate: '2026-07-30',
  durableObjects: { ROOMS: { className: 'NetplayRoom', useSQLite: true } },
  ratelimits: {
    ROOM_RATE_LIMIT: { namespace_id: '1', simple: { limit: 120, period: 60 } },
    ROOM_CREATE_LIMIT: { namespace_id: '2', simple: { limit: 6, period: 60 } },
  },
  bindings: { ALLOWED_ORIGINS: origin, REQUIRE_TURN: 'false' },
};
function client(mf) {
  return async (path, method = 'GET', body, auth, extra = {}) => {
    const response = await mf.dispatchFetch('https://service.test' + path, {
      method, headers: { Origin: origin, 'Content-Type': 'application/json',
        ...(auth ? { Authorization: `Bearer ${auth}` } : {}), ...extra },
      ...(body === undefined ? {} : { body: typeof body === 'string' ? body : JSON.stringify(body) }),
    });
    return { status: response.status, headers: response.headers,
      body: response.status === 204 ? null : await response.json() };
  };
}

test('real Worker runtime: rooms enforce roles, reserve one guest, exchange codes and expire on cancellation', async t => {
  const mf = new Miniflare(options); t.after(() => mf.dispose());
  const call = client(mf);
  assert.equal((await call('/rooms', 'POST', {}, null, { Origin: 'https://other.test' })).status, 403);
  const preflight = await call('/rooms', 'OPTIONS');
  assert.equal(preflight.status, 204);
  assert.equal(preflight.headers.get('Access-Control-Allow-Origin'), origin);
  assert.equal((await call('/rooms', 'POST', '{')).status, 400);
  assert.equal((await call('/rooms', 'POST', 'x'.repeat(103000))).status, 400);
  const created = await call('/rooms', 'POST', {});
  assert.equal(created.status, 201);
  assert.equal(created.headers.get('Cache-Control'), 'no-store');
  const { id, owner, expiresAt } = created.body;
  assert.match(id, /^[A-Za-z0-9_-]{22}$/);
  assert.notEqual(id, owner);
  assert.ok(expiresAt > Date.now() && expiresAt <= Date.now() + 900000);
  const path = `/rooms/${id}`;
  assert.equal((await call(path + '/join', 'POST', { guest })).status, 409);
  assert.equal((await call(path + '/offer', 'POST', { code: code('offer') }, id)).status, 403);
  assert.equal((await call(path + '/offer', 'POST', { code: code('answer') }, owner)).status, 400);
  assert.equal((await call(path + '/offer', 'POST', { code: code('offer') }, owner)).status, 200);
  const joins = await Promise.all([guest, 'h'.repeat(22)].map(value => call(path + '/join', 'POST', { guest: value })));
  assert.deepEqual(joins.map(value => value.status).sort(), [200, 409]);
  const winner = joins[0].status === 200 ? guest : 'h'.repeat(22);
  assert.equal((await call(path + '/join', 'POST', { guest: winner })).status, 200);
  assert.equal((await call(path + '/answer', 'GET', undefined, winner)).status, 404);
  assert.deepEqual((await call(path + '/answer', 'GET', undefined, owner)).body, { answer: null });
  assert.equal((await call(path + '/answer', 'POST', { code: code('answer') }, owner)).status, 404);
  assert.equal((await call(path + '/answer', 'POST', { code: code('answer') }, winner)).status, 200);
  assert.equal((await call(path + '/answer', 'GET', undefined, owner)).body.answer, code('answer'));
  assert.equal((await call(path, 'DELETE', undefined, winner)).status, 200);
  assert.equal((await call(path + '/answer', 'GET', undefined, owner)).status, 410);
});

test('production refuses to create a room without TURN; creation quota is separate from polling', async t => {
  const mf = new Miniflare({ ...options, bindings: { ...options.bindings, REQUIRE_TURN: 'true' } });
  t.after(() => mf.dispose());
  const call = client(mf);
  assert.equal((await call('/rooms', 'POST', {})).status, 503);
  for (let i = 0; i < 5; i++) await call('/rooms', 'POST', {});
  assert.equal((await call('/rooms', 'POST', {})).status, 429);
  assert.equal((await call(`/rooms/${guest}/answer`)).status, 410);
});

test('TURN credentials stay scoped to players and provider failures are redacted', async t => {
  let requests = 0;
  const mf = new Miniflare({ ...options,
    bindings: { ...options.bindings, REQUIRE_TURN: 'true', TURN_KEY_ID: 'private-key-id', TURN_KEY_API_TOKEN: 'private-api-token' },
    outboundService: async request => {
      requests++;
      assert.equal(request.headers.get('Authorization'), 'Bearer private-api-token');
      assert.deepEqual(await request.json(), { ttl: 86400 });
      if (requests > 1) return new Response('private-api-token provider failure', { status: 500 });
      return Response.json({ iceServers: [{ urls: ['turn:relay.test:3478'], username: 'temporary-user', credential: 'temporary-credential' }] });
    },
  });
  t.after(() => mf.dispose());
  const call = client(mf);
  const created = await call('/rooms', 'POST', {});
  assert.equal(created.status, 201);
  assert.equal(created.body.relay, true);
  assert.ok(!JSON.stringify(created.body).includes('private-'));
  const failed = await call('/rooms', 'POST', {});
  assert.equal(failed.status, 503);
  assert.ok(!JSON.stringify(failed.body).includes('private-'));
});
