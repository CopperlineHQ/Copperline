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
