// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { RoomClient, inviteUrl, roomFromInvite, signalingUrl } from './netplay-room.js';
const id = 'a'.repeat(22);
test('invitations retain only the page location and opaque room ID', () => {
  const url = inviteUrl(id, 'https://copperline.dev/try/?rom=private&token=secret#old');
  assert.equal(url, `https://copperline.dev/try/#room=${id}`);
  assert.equal(roomFromInvite(url), id);
  assert.equal(roomFromInvite(id), id);
  for (const value of ['bad', url + 'x', 'https://other.test/?room=' + id]) assert.equal(roomFromInvite(value), null);
  assert.throws(() => inviteUrl('bad'));
  for (const value of ['http://remote.test', 'https://u:p@remote.test', 'https://remote.test?secret=1']) assert.throws(() => signalingUrl(value));
  assert.equal(signalingUrl('http://127.0.0.1:8787/'), 'http://127.0.0.1:8787');
});
test('room client uses independent owner authentication and bounds response bodies without content length', async t => {
  const abort = new AbortController();
  const room = new RoomClient('https://service.test', abort.signal);
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    calls.push({ url, options });
    return Response.json({ id, owner: 'b'.repeat(22) });
  });
  await room.create();
  await room.publish('offer', 'test-code');
  assert.equal(calls[1].options.headers.Authorization, `Bearer ${'b'.repeat(22)}`);
  assert.equal(calls[0].options.credentials, 'omit');
  assert.equal(calls[0].options.referrerPolicy, 'no-referrer');
  t.mock.method(globalThis, 'fetch', async () => new Response('x'.repeat(129 * 1024)));
  await assert.rejects(room.request('/health'), /Invalid room response/);
});
test('waiting stops on cancellation and expired invitations', async t => {
  t.mock.method(globalThis, 'fetch', async () => Response.json({ answer: null }));
  const abort = new AbortController();
  const room = new RoomClient('https://service.test', abort.signal);
  await assert.rejects(room.waitForAnswer(Date.now() - 1), /expired/);
  const waiting = room.waitForAnswer(Date.now() + 900000);
  setTimeout(() => abort.abort(), 10);
  await assert.rejects(waiting, { name: 'AbortError' });
});
test('spectator invitations carry a separate capability and the watch client routes under /watch with polling backoff', async t => {
  const { watchInviteUrl, watchFromInvite } = await import('./netplay-room.js');
  const url = watchInviteUrl(id, 'https://copperline.dev/try/?rom=private#room=old');
  assert.equal(url, `https://copperline.dev/try/#watch=${id}`);
  assert.equal(watchFromInvite(url), id);
  assert.equal(watchFromInvite(id), id);
  assert.equal(roomFromInvite(url), null, 'a spectator link opens no player room');
  assert.equal(watchFromInvite(inviteUrl(id, 'https://copperline.dev/try/')), null);
  assert.throws(() => watchInviteUrl('bad'));
  const abort = new AbortController();
  const calls = [];
  const answers = [];
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    calls.push({ url, options });
    if (url.endsWith('/answer') && options.method === 'GET') return answers.shift();
    return Response.json({ id, owner: 'b'.repeat(22), expiresAt: Date.now() + 60000, iceServers: [], offers: [] });
  });
  const host = new RoomClient('https://service.test', abort.signal, '/watch');
  await host.create({ slots: 2 });
  assert.equal(calls[0].url, 'https://service.test/watch');
  assert.deepEqual(JSON.parse(calls[0].options.body), { slots: 2 });
  assert.deepEqual((await host.pollWatchOffers()).offers, []);
  const poll = calls.find(call => call.url.endsWith('/offers'));
  assert.equal(poll.url, `https://service.test/watch/${id}/offers`);
  assert.equal(poll.options.headers.Authorization, 'Bearer ' + 'b'.repeat(22));
  await host.answerWatch('c'.repeat(22), 'the-answer');
  const answered = calls.at(-1);
  assert.equal(answered.url, `https://service.test/watch/${id}/answer`);
  assert.deepEqual(JSON.parse(answered.options.body), { spectator: 'c'.repeat(22), code: 'the-answer' });
  await host.refuseWatch('d'.repeat(22));
  assert.equal(calls.at(-1).url, `https://service.test/watch/${id}/refuse`);
  assert.deepEqual(JSON.parse(calls.at(-1).options.body), { spectator: 'd'.repeat(22) });
  assert.equal(calls.at(-1).options.headers.Authorization, 'Bearer ' + 'b'.repeat(22));
  const spectator = new RoomClient('https://service.test', abort.signal, '/watch');
  await assert.rejects(spectator.join('bad'), /spectator invitation/);
  await spectator.join(id);
  assert.deepEqual(JSON.parse(calls.at(-1).options.body), { spectator: spectator.auth });
  assert.equal(calls.at(-1).url, `https://service.test/watch/${id}/join`);
  // A throttled poll backs off and retries instead of failing the join.
  spectator.wait = async () => {};
  answers.push(Response.json({ error: 'Too many requests' }, { status: 429 }),
    Response.json({ answer: null }), Response.json({ answer: 'the-answer' }));
  assert.equal(await spectator.waitForAnswer(Date.now() + 60000), 'the-answer');
  assert.equal(calls.filter(call => call.url.endsWith('/answer') && call.options.method === 'GET').length, 3);
  // A refusal ends the wait at once with the host's reason.
  answers.push(Response.json({ answer: null, refused: true }));
  await assert.rejects(spectator.waitForAnswer(Date.now() + 60000), /no free spectator places/);
  answers.push(Response.json({ error: 'gone' }, { status: 410 }));
  await assert.rejects(spectator.waitForAnswer(Date.now() + 60000), /gone/);
  await host.end();
  assert.equal(calls.at(-1).url, `https://service.test/watch/${id}`);
  assert.equal(calls.at(-1).options.method, 'DELETE');
});
