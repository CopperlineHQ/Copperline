// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { RtcWatch, RtcWatchPeer, SpectatorHub, feedMessage, FEED_CHUNK, FEED_BUFFER, FEED_TAKE, STALL_MS, INCOMING_LIMIT } from './netplay-watch.js';
import { decodeCode, encodeCode, validateWatchSettings } from './netplay-rtc.js';
import { RtcLink } from './netplay.js';

const description = type => ({ type, sdp: 'v=0\r\ns=-\r\n' });
const offered = { role: 'watch', build: 'build-1', media: 'host-v1', watch: 'watch-v1' };
const answered = { ...offered, controller: 'cd32' };
const players = { session: '0123456789abcdef0123456789abcdef', delay: 2, window: 8, controller: 'joystick' };

class Channel {
  constructor(label, options) { Object.assign(this, { label, ...options, bufferedAmount: 0, readyState: 'connecting', sent: [] }); }
  send(packet) { this.sent.push(packet); }
  close() { this.readyState = 'closed'; }
}
class Peer extends EventTarget {
  constructor() { super(); this.iceGatheringState = 'complete'; this.connectionState = 'new'; }
  createDataChannel(label, options) { return new Channel(label, options); }
  async createOffer() { return description('offer'); }
  async createAnswer() { return description('answer'); }
  async setLocalDescription(value) { this.localDescription = value; }
  async setRemoteDescription(value) { this.remoteDescription = value; }
  setConfiguration(configuration) { this.configuration = configuration; }
  close() { this.connectionState = 'closed'; }
}
const identity = new Uint8Array(32).fill(9);
const settle = () => new Promise(resolve => setTimeout(resolve, 5));

test('spectator settings carry the build and channel versions; only the host answer names the controller', () => {
  assert.deepEqual(validateWatchSettings(offered), offered);
  assert.deepEqual(validateWatchSettings(answered), answered);
  for (const bad of [{ ...offered, build: '' }, { ...offered, build: 'x'.repeat(129) }, { ...offered, watch: 'watch-v2' },
    { ...offered, media: 'unknown' }, { ...offered, controller: 'analogue' }, { ...offered, role: 'play' }, players]) {
    assert.throws(() => validateWatchSettings(bad));
  }
  const code = encodeCode(description('offer'), offered);
  assert.deepEqual(decodeCode(code, 'offer').settings, offered);
  assert.deepEqual(decodeCode(encodeCode(description('answer'), answered), 'answer').settings, answered);
});

test('a spectator offers two reliable channels, verifies once, reports frames and feeds every chunk to the core', async () => {
  const link = new RtcWatch({ PeerConnection: Peer });
  const code = await link.offer(offered);
  assert.equal(link.channel.label, 'copperline-watch-v1');
  assert.equal(link.channel.ordered, true);
  assert.equal(link.channel.maxRetransmits, undefined);
  assert.equal(link.media.channel.label, 'copperline-setup-v1');
  assert.equal(decodeCode(code, 'offer').settings.role, 'watch');
  await assert.rejects(link.accept(encodeCode(description('answer'), offered)), /different/);
  await assert.rejects(link.accept(encodeCode(description('answer'), { ...answered, build: 'build-2' })), /different/);
  await assert.rejects(link.accept(encodeCode(description('answer'), players)), /different/);
  await link.accept(encodeCode(description('answer'), answered));
  assert.equal(link.settings.controller, 'cd32');
  const channel = link.channel;
  channel.readyState = 'open';
  const received = [];
  const emu = { spectate_identity: () => identity, spectate_status: () => [1, 77, 0, 60, 0], spectate_receive: bytes => received.push(...bytes) };
  link.send(emu, 0);
  assert.deepEqual(channel.sent[0], feedMessage(5, identity));
  link.send(emu, 500);
  assert.equal(channel.sent.length, 1, 'frame reports wait a second');
  link.send(emu, 1000);
  assert.equal(channel.sent.length, 2);
  assert.equal(channel.sent[1][0], 6);
  assert.equal(new DataView(channel.sent[1].buffer).getBigUint64(5, true), 77n);
  channel.onmessage({ data: new Uint8Array([1, 2]).buffer });
  channel.onmessage({ data: new Uint8Array([3]).buffer });
  link.receive(emu);
  assert.deepEqual(received, [1, 2, 3]);
  channel.onmessage({ data: new ArrayBuffer(FEED_CHUNK + 1) });
  assert.equal(link.closed, true);
  const flooded = new RtcWatch({ PeerConnection: Peer });
  await flooded.offer(offered);
  flooded.channel.readyState = 'open';
  for (let i = 0; i <= INCOMING_LIMIT / FEED_CHUNK; i++) flooded.channel.onmessage({ data: new ArrayBuffer(FEED_CHUNK) });
  assert.equal(flooded.closed, true, 'an undrained backlog closes the link');
  const unexpected = new RtcWatch({ PeerConnection: Peer });
  unexpected.pc.ondatachannel({ channel: new Channel('copperline-netplay-v1', { ordered: false, maxRetransmits: 0 }) });
  assert.equal(unexpected.closed, true, 'the host opens no channel toward a spectator');
  const player = new RtcLink({ PeerConnection: Peer });
  await assert.rejects(player.answer(encodeCode(description('offer'), offered)), /spectator/);
});

function hostPeer(overrides = {}, feed = [new Uint8Array(FEED_CHUNK * 2 + 5).fill(1)]) {
  const events = { opened: 0, closed: null, cursors: [], taken: [] };
  const emu = {
    netplay_identity: () => identity,
    spectator_feed_open: () => { events.opened++; return 4; },
    spectator_feed_close: id => events.cursors.push(id),
    spectator_feed_take: (id, max) => { events.taken.push([id, max]); return feed.shift() ?? new Uint8Array(); },
  };
  const peer = new RtcWatchPeer({ token: 'spectator', PeerConnection: Peer, build: 'build-1', controller: 'joystick',
    media: () => ({ manifest: { type: 'host-v1', config: {}, files: [] }, media: [] }), machine: () => emu,
    onClose: reason => { events.closed = reason; }, ...overrides });
  return { peer, emu, events };
}

test('the host peer answers matching builds, streams after verification with bounded buffering, and drops a stalled spectator', async () => {
  const { peer, emu, events } = hostPeer();
  await assert.rejects(peer.answer(encodeCode(description('offer'), { ...offered, build: 'build-2' })), /build/);
  await assert.rejects(peer.answer(encodeCode(description('offer'), players)), /spectator/);
  const answer = await peer.answer(encodeCode(description('offer'), offered));
  assert.equal(decodeCode(answer, 'answer').settings.controller, 'joystick');
  const media = new Channel('copperline-setup-v1', { ordered: true });
  media.readyState = 'open';
  peer.pc.ondatachannel({ channel: media });
  await settle();
  assert.equal(peer.state, 'media');
  assert.ok(media.sent.length >= 1, 'the prepared manifest goes out without rehashing');
  const watch = new Channel('copperline-watch-v1', { ordered: true });
  watch.readyState = 'open';
  peer.pc.ondatachannel({ channel: watch });
  assert.equal(peer.channel, watch);
  watch.onmessage({ data: feedMessage(6, new Uint8Array(8)).buffer });
  assert.equal(peer.closed, false, 'a frame report before verification is harmless');
  watch.onmessage({ data: feedMessage(5, identity).buffer });
  assert.equal(peer.state, 'streaming');
  assert.equal(events.opened, 1);
  peer.pump(emu, 1000);
  assert.deepEqual(events.taken, [[4, FEED_TAKE], [4, FEED_TAKE]]);
  assert.equal(watch.sent.length, 3);
  assert.equal(watch.sent[0].length, FEED_CHUNK);
  assert.equal(watch.sent[2].length, 5);
  assert.equal(peer.pending, null);
  const status = new Uint8Array(13);
  status[0] = 6; new DataView(status.buffer).setUint32(1, 8, true); new DataView(status.buffer).setBigUint64(5, 123n, true);
  watch.onmessage({ data: status.buffer });
  assert.equal(peer.frame, 123);
  watch.bufferedAmount = FEED_BUFFER + 1;
  peer.pump(emu, 2000);
  assert.equal(watch.sent.length, 3, 'a full buffer sends nothing more');
  peer.pump(emu, 2000 + STALL_MS + 1);
  assert.equal(peer.closed, true);
  assert.match(events.closed, /stalled/);
  assert.deepEqual(events.cursors, [4], 'the feed cursor is released');
  const mismatch = hostPeer();
  await mismatch.peer.answer(encodeCode(description('offer'), offered));
  const other = new Channel('copperline-watch-v1', { ordered: true });
  mismatch.peer.pc.ondatachannel({ channel: other });
  mismatch.peer.state = 'verifying';
  other.onmessage({ data: feedMessage(5, new Uint8Array(32).fill(8)).buffer });
  assert.equal(mismatch.peer.closed, true);
  assert.match(mismatch.events.closed, /different machine/);
  assert.equal(mismatch.events.opened, 0);
  const garbage = hostPeer();
  const bad = new Channel('copperline-watch-v1', { ordered: true });
  garbage.peer.pc.ondatachannel({ channel: bad });
  bad.onmessage({ data: new Uint8Array([1, 2, 3]).buffer });
  assert.equal(garbage.peer.closed, true);
  const unordered = hostPeer();
  unordered.peer.pc.ondatachannel({ channel: new Channel('copperline-watch-v1', { ordered: false, maxRetransmits: 0 }) });
  assert.equal(unordered.peer.closed, true);
});

test('a whole disk image is sent chunk by chunk across pumps, never past the buffer bound', async () => {
  // Two buffers: 40 chunks (a disk change returned whole), then 2 more.
  const image = new Uint8Array(FEED_CHUNK * 40).fill(7);
  const tail = new Uint8Array(FEED_CHUNK * 2).fill(8);
  const { peer, emu } = hostPeer({}, [image, tail]);
  await peer.answer(encodeCode(description('offer'), offered));
  const watch = new Channel('copperline-watch-v1', { ordered: true });
  watch.readyState = 'open';
  watch.send = packet => { watch.sent.push(packet); watch.bufferedAmount += packet.length; };
  peer.pc.ondatachannel({ channel: watch });
  peer.state = 'verifying';
  watch.onmessage({ data: feedMessage(5, identity).buffer });
  const perPump = FEED_BUFFER / FEED_CHUNK;
  peer.pump(emu, 1000);
  assert.equal(watch.sent.length, perPump, 'stops once the buffer bound is reached');
  assert.equal(watch.bufferedAmount, FEED_BUFFER);
  assert.equal(peer.offset, perPump * FEED_CHUNK);
  assert.ok(peer.pending === image);
  watch.bufferedAmount = FEED_BUFFER + 1;
  peer.pump(emu, 1001);
  assert.equal(watch.sent.length, perPump, 'a full buffer sends nothing more');
  watch.bufferedAmount = 0;
  peer.pump(emu, 1002);
  assert.equal(watch.sent.length, 2 * perPump);
  watch.bufferedAmount = 0;
  peer.pump(emu, 1003);
  assert.equal(watch.sent.length, 40 + 2, 'the rest of the image, then the next buffer');
  assert.equal(peer.pending, null);
  assert.equal(peer.sent, image.length + tail.length);
  assert.ok(watch.sent.every(packet => packet.length <= FEED_CHUNK));
  assert.deepEqual(Buffer.concat(watch.sent.map(packet => Buffer.from(packet))), Buffer.concat([Buffer.from(image), Buffer.from(tail)]));
  // A channel that never reports its buffer still gets a bounded pump.
  const silent = hostPeer({}, [new Uint8Array(FEED_CHUNK * 100)]);
  await silent.peer.answer(encodeCode(description('offer'), offered));
  const quiet = new Channel('copperline-watch-v1', { ordered: true });
  quiet.readyState = 'open';
  silent.peer.pc.ondatachannel({ channel: quiet });
  silent.peer.state = 'verifying';
  quiet.onmessage({ data: feedMessage(5, identity).buffer });
  silent.peer.pump(silent.emu, 1000);
  assert.equal(quiet.sent.length, FEED_BUFFER / FEED_CHUNK);
});

test('the hub answers offers up to its places, refuses the rest, hashes media once, and closes every peer with the room', async t => {
  const offer = encodeCode(description('offer'), offered);
  const room = { id: 'r'.repeat(22), polled: 0, answers: [], refused: [], ended: false,
    pollWatchOffers: async () => { room.polled++; return { offers: ['a', 'b', 'c'].map(letter => ({ spectator: letter.repeat(22), code: offer })) }; },
    answerWatch: async spectator => { room.answers.push(spectator); },
    refuseWatch: async spectator => { room.refused.push(spectator); },
    end: () => { room.ended = true; } };
  const snapshot = { model: 'A500', video: 'PAL', floppySpeed: 100, floppySounds: true, monoAudio: false, build: 'build-1',
    rom: { rom: new Uint8Array(64).fill(1), ext: null, label: 'rom' }, disks: [null, null, null, null] };
  let machine = null;
  const hub = new SpectatorHub({ room, iceServers: [], slots: 2, build: 'build-1', controller: 'joystick',
    media: () => snapshot, machine: () => machine, PeerConnection: Peer });
  t.after(() => hub.close());
  hub.start();
  await settle();
  assert.equal(room.polled, 0, 'the room is not polled before the host machine runs');
  assert.equal(hub.peers.size, 0);
  machine = { netplay_identity: () => identity };
  clearTimeout(hub.timer);
  await hub.poll();
  assert.equal(room.polled, 1);
  assert.deepEqual(room.answers, ['a'.repeat(22), 'b'.repeat(22)]);
  assert.deepEqual(room.refused, ['c'.repeat(22)], 'a full house turns the surplus offer away');
  assert.equal(hub.peers.size, 2);
  assert.equal(hub.watching, 0);
  assert.equal(hub.invitation, room.id);
  assert.deepEqual([...hub.peers.values()][0].pc.configuration, { iceServers: [], iceTransportPolicy: 'all' });
  await hub.poll();
  assert.equal(room.answers.length, 2, 'known offers are not answered again');
  assert.equal(room.refused.length, 2, 'a still-listed surplus offer is refused again, never answered');
  const prepared = await hub.media();
  assert.equal(prepared.manifest.files[0].kind, 'rom');
  assert.equal(await hub.media(), prepared, 'media is described once for every spectator');
  const peers = [...hub.peers.values()];
  hub.close();
  assert.equal(hub.closed, true);
  assert.equal(room.ended, true);
  assert.equal(hub.peers.size, 0);
  assert.ok(peers.every(peer => peer.closed));
  assert.equal(hub.timer, null);
});

test('an expired room withdraws its invitation but keeps the spectators it admitted', async t => {
  const offer = encodeCode(description('offer'), offered);
  const room = { id: 'w'.repeat(22), polls: 0, ended: false,
    pollWatchOffers: async () => {
      if (++room.polls > 1) throw new Error('This invitation has expired or ended. Ask the host for a new link.');
      return { offers: [{ spectator: 'a'.repeat(22), code: offer }] };
    },
    answerWatch: async () => {},
    refuseWatch: async () => {},
    end: () => { room.ended = true; } };
  const notices = [];
  const seen = [];
  const machine = { netplay_identity: () => identity };
  const hub = new SpectatorHub({ room, slots: 3, build: 'build-1', controller: 'joystick', media: () => ({}),
    machine: () => machine, status: text => notices.push(text), changed: () => seen.push(hub.invitation), PeerConnection: Peer });
  t.after(() => hub.close());
  assert.equal(hub.invitation, room.id);
  hub.start();
  clearTimeout(hub.timer);
  await hub.poll();
  assert.equal(hub.peers.size, 1);
  await hub.poll();
  assert.deepEqual([hub.closed, hub.invitation, hub.peers.size, room.ended], [true, null, 1, true]);
  assert.match(notices.at(-1), /invitation ended/);
  assert.equal(seen.at(-1), null, 'the panel is told once the invitation is dead');
  assert.ok([...hub.peers.values()].every(peer => !peer.closed), 'admitted spectators keep watching');
});
