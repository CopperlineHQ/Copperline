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
      return Response.json({ iceServers: [
        { urls: 'stun:relay.test:53' },
        { urls: ['stun:relay.test:3478', 'stun:relay.test:53'] },
        { urls: ['turn:relay.test:3478?transport=udp', 'turn:relay.test:53?transport=udp',
          'turns:relay.test:5349?transport=tcp', 'turns:relay.test:443?transport=tcp'],
          username: 'temporary-user', credential: 'temporary-credential' },
      ] });
    },
  });
  t.after(() => mf.dispose());
  const call = client(mf);
  const created = await call('/rooms', 'POST', {});
  assert.equal(created.status, 201);
  assert.equal(created.body.relay, true);
  assert.deepEqual(created.body.iceServers, [
    { urls: ['stun:relay.test:3478'] },
    { urls: ['turn:relay.test:3478?transport=udp', 'turns:relay.test:5349?transport=tcp',
      'turns:relay.test:443?transport=tcp'], username: 'temporary-user', credential: 'temporary-credential' },
  ]);
  assert.ok(!JSON.stringify(created.body).includes('private-'));
  const failed = await call('/rooms', 'POST', {});
  assert.equal(failed.status, 503);
  assert.ok(!JSON.stringify(failed.body).includes('private-'));
});

test('watch rooms admit spectators by their own capability, relay offers to the owner and free places once answered', async t => {
  const mf = new Miniflare(options); t.after(() => mf.dispose());
  const call = client(mf);
  assert.equal((await call('/watch', 'POST', {})).status, 400);
  assert.equal((await call('/watch', 'POST', { slots: 9 })).status, 400);
  assert.equal((await call('/watch', 'POST', { slots: 1, extra: true })).status, 400);
  const created = await call('/watch', 'POST', { slots: 2 });
  assert.equal(created.status, 201);
  const { id, owner, expiresAt } = created.body;
  assert.match(id, /^[A-Za-z0-9_-]{22}$/);
  assert.notEqual(id, owner);
  const path = `/watch/${id}`;
  // The watch capability opens no player room, and a player room's id no
  // watch room: each kind has its own namespace and record.
  assert.equal((await call(`/rooms/${id}/join`, 'POST', { guest })).status, 410);
  const room = await call('/rooms', 'POST', {});
  assert.equal((await call(`/watch/${room.body.id}/join`, 'POST', { spectator: guest })).status, 410);
  assert.equal((await call(`/rooms/${room.body.id}/offer`, 'POST', { code: code('offer') }, room.body.owner)).status, 200);
  // Two places: a third spectator waits until one place is answered and fetched.
  const spectators = ['s'.repeat(22), 't'.repeat(22), 'u'.repeat(22)];
  assert.equal((await call(path + '/join', 'POST', { spectator: 'bad' })).status, 400);
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[0] })).status, 200);
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[1] })).status, 200);
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[2] })).status, 409);
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[0] })).status, 200, 'rejoining is idempotent');
  // Offers need the spectator's own token; the owner sees offered, unanswered entries.
  assert.equal((await call(path + '/offer', 'POST', { code: code('offer') })).status, 403);
  assert.equal((await call(path + '/offer', 'POST', { code: code('offer') }, owner)).status, 403, 'the owner offers nothing');
  assert.equal((await call(path + '/offer', 'POST', { code: code('answer') }, spectators[0])).status, 400);
  assert.equal((await call(path + '/offer', 'POST', { code: code('offer') }, spectators[0])).status, 200);
  assert.equal((await call(path + '/offers', 'GET', undefined, spectators[0])).status, 403);
  await new Promise(resolve => setTimeout(resolve, 5));
  const offers = await call(path + '/offers', 'GET', undefined, owner);
  assert.equal(offers.status, 200);
  assert.deepEqual(offers.body.offers, [{ spectator: spectators[0], code: code('offer') }]);
  assert.ok(offers.body.expiresAt > expiresAt, 'an owner poll extends the room');
  // Answers: unknown spectators and wrong code types are refused; the
  // spectator fetches its answer exactly once.
  assert.equal((await call(path + '/answer', 'POST', { spectator: spectators[2], code: code('answer') }, owner)).status, 404);
  assert.equal((await call(path + '/answer', 'POST', { spectator: spectators[1], code: code('answer') }, owner)).status, 404, 'no offer yet');
  assert.equal((await call(path + '/answer', 'POST', { spectator: spectators[0], code: code('offer') }, owner)).status, 400);
  assert.equal((await call(path + '/answer', 'POST', { spectator: spectators[0], code: code('answer') }, spectators[0])).status, 403);
  assert.equal((await call(path + '/answer', 'POST', { spectator: spectators[0], code: code('answer') }, owner)).status, 200);
  assert.deepEqual((await call(path + '/offers', 'GET', undefined, owner)).body.offers, []);
  assert.equal((await call(path + '/answer', 'GET', undefined, spectators[1])).body.answer, null);
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[2] })).status, 409, 'an unfetched answer still holds its place');
  assert.equal((await call(path + '/answer', 'GET', undefined, spectators[0])).body.answer, code('answer'));
  assert.equal((await call(path + '/answer', 'GET', undefined, spectators[0])).body.answer, code('answer'), 'a lost response can be retried');
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[2] })).status, 200, 'a fetched answer frees the place');
  assert.equal((await call(path + '/join', 'POST', { spectator: 'v'.repeat(22) })).status, 409);
  assert.equal((await call(path + '/slots', 'POST', { slots: 4 }, owner)).status, 404, 'rooms are not resized');
  // A full host turns an offer away: the spectator reads the refusal on its
  // next poll, its place is freed, and the offer leaves the owner's list.
  // Spectator 2 (answered, fetched) and the third one (waiting) hold the
  // two places, so the refused one joined while a place was free.
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[2] })).status, 200, 'a joined spectator keeps its place');
  assert.equal((await call(path + '/answer', 'GET', undefined, spectators[1])).body.answer, null);
  assert.equal((await call(path + '/answer', 'POST', { spectator: spectators[1], code: code('answer') }, owner)).status, 404, 'still no offer from the second');
  assert.equal((await call(path + '/offer', 'POST', { code: code('offer') }, spectators[1])).status, 200);
  assert.equal((await call(path + '/answer', 'POST', { spectator: spectators[1], code: code('answer') }, owner)).status, 200);
  assert.equal((await call(path + '/answer', 'GET', undefined, spectators[1])).body.answer, code('answer'));
  assert.equal((await call(path + '/join', 'POST', { spectator: 'v'.repeat(22) })).status, 200, 'a fetched answer frees its place');
  assert.equal((await call(path + '/offer', 'POST', { code: code('offer') }, 'v'.repeat(22))).status, 200);
  assert.equal((await call(path + '/refuse', 'POST', { spectator: 'v'.repeat(22) }, 'v'.repeat(22))).status, 403);
  assert.equal((await call(path + '/refuse', 'POST', { spectator: 'bad' }, owner)).status, 400);
  assert.equal((await call(path + '/refuse', 'POST', { spectator: spectators[2] }, owner)).status, 404, 'nothing offered yet');
  assert.equal((await call(path + '/refuse', 'POST', { spectator: spectators[0] }, owner)).status, 409, 'already answered');
  assert.deepEqual((await call(path + '/offers', 'GET', undefined, owner)).body.offers.map(offer => offer.spectator), ['v'.repeat(22)]);
  assert.equal((await call(path + '/refuse', 'POST', { spectator: 'v'.repeat(22) }, owner)).status, 200);
  assert.deepEqual((await call(path + '/offers', 'GET', undefined, owner)).body.offers, [], 'a refused offer is not listed again');
  const refused = await call(path + '/answer', 'GET', undefined, 'v'.repeat(22));
  assert.equal(refused.status, 200);
  assert.deepEqual([refused.body.answer, refused.body.refused], [null, true]);
  assert.equal((await call(path + '/answer', 'GET', undefined, spectators[2])).body.refused, false);
  assert.equal((await call(path + '/join', 'POST', { spectator: 'w'.repeat(22) })).status, 200, 'a refusal frees its place');
  assert.equal((await call(path + '/join', 'POST', { spectator: 'x'.repeat(22) })).status, 409, 'the waiting third spectator and the newcomer fill both places');
  // Only the owner ends the room.
  assert.equal((await call(path, 'DELETE', undefined, spectators[1])).status, 403);
  assert.equal((await call(path, 'DELETE', undefined, owner)).status, 200);
  assert.equal((await call(path + '/join', 'POST', { spectator: spectators[1] })).status, 410);
  assert.equal((await call('/health')).body.version, 2);
});

test('TURN credentials are issued once per spectator place', async t => {
  let requests = 0;
  const mf = new Miniflare({ ...options,
    bindings: { ...options.bindings, REQUIRE_TURN: 'true', TURN_KEY_ID: 'private-key-id', TURN_KEY_API_TOKEN: 'private-api-token' },
    outboundService: async () => {
      requests++;
      return Response.json({ iceServers: [{ urls: ['turn:relay.test:3478?transport=udp'], username: 'u', credential: 'c' }] });
    },
  });
  t.after(() => mf.dispose());
  const call = client(mf);
  const created = await call('/watch', 'POST', { slots: 3 });
  assert.equal(created.status, 201);
  assert.equal(created.body.relay, true);
  assert.equal(requests, 1);
  const path = `/watch/${created.body.id}`;
  const joined = await call(path + '/join', 'POST', { spectator: guest });
  assert.equal(joined.status, 200);
  assert.equal(joined.body.relay, true);
  assert.equal(joined.body.iceServers[0].username, 'u');
  assert.equal(requests, 2);
  assert.equal((await call(path + '/join', 'POST', { spectator: guest })).status, 200);
  assert.equal(requests, 2, 'rejoining reuses the place and its credentials');
  assert.equal((await call(path + '/join', 'POST', { spectator: 'h'.repeat(22) })).status, 200);
  assert.equal(requests, 3);
  assert.ok(!JSON.stringify(joined.body).includes('private-'));
});
