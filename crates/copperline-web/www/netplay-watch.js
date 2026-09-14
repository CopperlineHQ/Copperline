// SPDX-License-Identifier: GPL-3.0-or-later
// Spectators follow the host's confirmed frames over one reliable channel.
// Each spectator offers; the host answers from the watch room, sends its
// cold-boot media, checks the spectator's machine fingerprint, then streams
// the feed the core encodes. A spectator never sends input.
import { RtcCommon, WATCH_CHANNEL, decodeCode, validateWatchSettings } from './netplay-rtc.js';
import { MEDIA_CHANNEL, MediaTransfer, describeMedia } from './netplay-media.js';

export const FEED_CHUNK = 16 * 1024;
export const FEED_BUFFER = 256 * 1024;
export const FEED_TAKE = 64 * 1024;
export const STALL_MS = 30000;
export const INCOMING_LIMIT = 4 * 1024 * 1024;
export const POLL_MS = 2000;
const STATUS_MS = 1000;
const KIND_VERIFIED = 5;
const KIND_STATUS = 6;

// A feed message: kind, little-endian length, payload.
export function feedMessage(kind, payload) {
  const bytes = new Uint8Array(5 + payload.length);
  bytes[0] = kind;
  new DataView(bytes.buffer).setUint32(1, payload.length, true);
  bytes.set(payload, 5);
  return bytes;
}

const reliable = channel => channel.ordered === true && channel.maxRetransmits == null && channel.maxPacketLifeTime == null;
const now = () => performance.now();

// The spectator's side: creates both channels, receives the host's media,
// reports its fingerprint, then feeds every chunk to the core.
export class RtcWatch extends RtcCommon {
  constructor({ iceServers = [], onOpen = () => {}, onClose = () => {}, PeerConnection } = {}) {
    super({ iceServers, PeerConnection });
    this.host = false;
    this.spectate = true;
    this.incoming = [];
    this.incomingBytes = 0;
    this.opened = false;
    this.onOpen = onOpen;
    this.onClose = onClose;
    this.media = null;
    this.verified = false;
    this.lastStatus = 0;
    this.mediaReady = new Promise((resolve, reject) => { this.mediaAttached = resolve; this.mediaFailed = reject; });
    this.mediaReady.catch(() => {});
    this.pc.ondatachannel = event => { event.channel.close(); this.close('Unexpected channel from the host. Refresh both pages.'); };
  }

  attach(channel) {
    if (channel.label === MEDIA_CHANNEL) {
      this.media = new MediaTransfer(channel, { host: false, fail: error => this.close(error.message) });
      this.mediaAttached(this.media);
      return;
    }
    this.channel = channel;
    channel.binaryType = 'arraybuffer';
    channel.onmessage = event => {
      if (this.closed) return;
      if (!(event.data instanceof ArrayBuffer) || !event.data.byteLength || event.data.byteLength > FEED_CHUNK) {
        this.close('Invalid spectator feed');
        return;
      }
      this.incomingBytes += event.data.byteLength;
      if (this.incomingBytes > INCOMING_LIMIT) { this.close('Spectator feed overflowed'); return; }
      this.incoming.push(new Uint8Array(event.data));
    };
    channel.onopen = () => {
      if (this.closed || this.opened) return;
      this.opened = true;
      this.diagnostics.record('data-open', this.pc);
      this.diagnostics.capture(this.pc);
      clearTimeout(this.timer);
      Promise.resolve().then(() => this.closed ? undefined : this.onOpen(this))
        .catch(error => { console.error('Spectator startup failed', error); this.close(String(error.message ?? error)); });
    };
    channel.onclose = () => {
      this.diagnostics.record('data-close', this.pc);
      this.close('The host ended the game. Copy diagnostics for a connection report.');
    };
    channel.onerror = event => {
      this.diagnostics.record('data-error', this.pc);
      console.error('Spectator feed channel failed', event.error ?? event);
      this.close(`Spectator feed channel failed${event.error?.message ? ': ' + event.error.message : ''}`);
    };
  }

  async offer(settings) {
    this.settings = validateWatchSettings(settings);
    this.attach(this.pc.createDataChannel(WATCH_CHANNEL, { ordered: true }));
    this.attach(this.pc.createDataChannel(MEDIA_CHANNEL, { ordered: true }));
    return this.gather(await this.pc.createOffer());
  }

  async accept(code) {
    const { description, settings } = decodeCode(code, 'answer');
    const mine = this.settings;
    if (settings.role !== 'watch' || settings.build !== mine.build || settings.media !== mine.media ||
        settings.watch !== mine.watch || !settings.controller) {
      throw new Error('Answer belongs to a different spectator session or version. Refresh both pages and try again.');
    }
    this.settings = settings;
    await this.pc.setRemoteDescription(description);
    if (this.closed) throw new Error('Connection cancelled');
    if (!this.opened) {
      clearTimeout(this.timer);
      this.timer = setTimeout(() => this.close('Host connection timed out. Copy diagnostics, then ask for a new invitation.'), 60000);
    }
  }

  async transferMedia(_snapshot, progress) {
    if (this.closed) throw new Error('Game setup transfer cancelled');
    this.timer = setTimeout(() => this.close('Game setup transfer timed out. Ask for a new invitation.'), 180000);
    try {
      const transfer = await this.mediaReady;
      transfer.progress = progress;
      return await transfer.receive();
    } finally { clearTimeout(this.timer); }
  }

  receive(emu) {
    for (const bytes of this.incoming.splice(0)) emu.spectate_receive(bytes);
    this.incomingBytes = 0;
  }

  // The fingerprint first, then a frame report every second so the host
  // can tell a stalled spectator from a slow one.
  send(emu, at = now()) {
    if (this.closed || this.channel?.readyState !== 'open') return;
    if (!this.verified) {
      const identity = emu.spectate_identity();
      if (identity.length !== 32) return;
      this.channel.send(feedMessage(KIND_VERIFIED, identity));
      this.verified = true;
      this.lastStatus = at;
      return;
    }
    if (at - this.lastStatus < STATUS_MS) return;
    const payload = new Uint8Array(8);
    new DataView(payload.buffer).setBigUint64(0, BigInt(Math.max(0, Math.floor(emu.spectate_status()[1] ?? 0))), true);
    this.channel.send(feedMessage(KIND_STATUS, payload));
    this.lastStatus = at;
  }

  close(reason = 'Disconnected. Open an invitation to watch again.') {
    if (this.closed) return;
    this.closed = true;
    this.diagnostics.record('stopped', this.pc);
    this.diagnostics.capture(this.pc);
    clearTimeout(this.timer);
    this.cancelGather?.();
    this.mediaFailed(new Error('Game setup transfer cancelled'));
    try { this.onClose(reason, this); }
    finally { this.dispose(); }
  }

  dispose() {
    if (this.media) {
      const channel = this.media.channel;
      channel.onopen = channel.onmessage = channel.onclose = channel.onerror = null;
      this.media.stop();
      channel.close();
      this.media = null;
    }
    this.mediaReady = this.mediaAttached = this.mediaFailed = null;
    this.incoming.length = 0;
    this.disposePeer();
  }
}

// The host's side of one spectator: answers its offer, serves the media,
// checks the fingerprint and streams the feed with bounded buffering.
export class RtcWatchPeer extends RtcCommon {
  constructor({ token, iceServers = [], PeerConnection, build, controller, media, machine, onClose = () => {} }) {
    super({ iceServers, PeerConnection });
    Object.assign(this, { token, build, controller, media, machine, onClose });
    this.host = true;
    this.state = 'signaling';
    this.transfer = null;
    this.cursor = null;
    this.pending = null;
    this.offset = 0;
    this.lastDrain = 0;
    this.sent = 0;
    this.frame = 0;
    this.pc.ondatachannel = event => this.attach(event.channel);
  }

  attach(channel) {
    if (this.closed) { channel.close(); return; }
    if (channel.label === MEDIA_CHANNEL && !this.transfer && reliable(channel)) {
      this.transfer = new MediaTransfer(channel, { host: true, fail: error => this.close(error.message) });
      this.state = 'media';
      Promise.resolve(this.media()).then(prepared => this.transfer.send(null, prepared))
        .then(() => { if (!this.closed && this.state === 'media') this.state = 'verifying'; })
        .catch(error => this.close(String(error.message ?? error)));
      return;
    }
    if (channel.label === WATCH_CHANNEL && !this.channel && reliable(channel)) {
      this.channel = channel;
      channel.binaryType = 'arraybuffer';
      channel.bufferedAmountLowThreshold = FEED_BUFFER / 2;
      channel.onmessage = event => {
        try { this.accept(event.data); } catch (error) { this.close(String(error.message ?? error)); }
      };
      channel.onclose = () => this.close('Spectator disconnected');
      channel.onerror = () => this.close('Spectator connection failed');
      return;
    }
    channel.close();
    this.close('Unexpected spectator channel');
  }

  async answer(code) {
    const { description, settings } = decodeCode(code, 'offer');
    if (settings.role !== 'watch') throw new Error('Not a spectator offer');
    if (settings.build !== this.build) throw new Error('Spectator uses a different emulator build');
    this.settings = { ...settings, controller: this.controller };
    await this.pc.setRemoteDescription(description);
    if (this.closed) throw new Error('Spectator connection cancelled');
    return this.gather(await this.pc.createAnswer());
  }

  accept(data) {
    if (!(data instanceof ArrayBuffer) || data.byteLength < 5 || data.byteLength > 5 + 32) throw new Error('Invalid spectator message');
    const bytes = new Uint8Array(data);
    const view = new DataView(data);
    if (view.getUint32(1, true) !== data.byteLength - 5) throw new Error('Invalid spectator message');
    if (bytes[0] === KIND_VERIFIED && data.byteLength === 5 + 32) {
      // The spectator boots as soon as its media is verified; its report
      // may land before this side saw the acknowledgement.
      if (!['media', 'verifying'].includes(this.state)) throw new Error('Unexpected spectator verification');
      const emu = this.machine();
      if (!emu) throw new Error('The host machine is not running');
      const expected = emu.netplay_identity();
      if (expected.length !== 32 || !bytes.subarray(5).every((byte, i) => byte === expected[i])) {
        throw new Error('Spectator built a different machine');
      }
      this.cursor = emu.spectator_feed_open();
      this.state = 'streaming';
      this.lastDrain = now();
      return;
    }
    if (bytes[0] === KIND_STATUS && data.byteLength === 5 + 8) {
      this.frame = Number(view.getBigUint64(5, true));
      return;
    }
    throw new Error('Invalid spectator message');
  }

  // Send whatever the cursor still needs while the channel drains. A taken
  // buffer (a whole disk image, at most) is kept on the peer and sent one
  // chunk at a time across pumps, so no more than FEED_BUFFER plus one
  // chunk is ever queued; a spectator whose buffer never drains is dropped
  // rather than throttling the host.
  pump(emu, at = now()) {
    if (this.state !== 'streaming' || this.closed || this.channel?.readyState !== 'open') return;
    if (this.channel.bufferedAmount > FEED_BUFFER) {
      if (at - this.lastDrain > STALL_MS) this.close('Spectator stalled');
      return;
    }
    this.lastDrain = at;
    // A channel that never reports its buffer still gets a bounded pump.
    for (let chunks = 0; chunks < FEED_BUFFER / FEED_CHUNK && this.channel.bufferedAmount <= FEED_BUFFER; chunks++) {
      if (!this.pending) {
        const out = emu.spectator_feed_take(this.cursor, FEED_TAKE);
        if (!out.length) return;
        this.pending = out;
        this.offset = 0;
      }
      const end = Math.min(this.offset + FEED_CHUNK, this.pending.length);
      this.channel.send(this.pending.subarray(this.offset, end));
      this.sent += end - this.offset;
      this.offset = end;
      if (this.offset >= this.pending.length) this.pending = null;
    }
  }

  close(reason = 'Spectator disconnected') {
    if (this.closed) return;
    this.closed = true;
    this.diagnostics.record('stopped', this.pc);
    clearTimeout(this.timer);
    this.cancelGather?.();
    if (this.cursor !== null) {
      try { this.machine()?.spectator_feed_close(this.cursor); } catch {}
      this.cursor = null;
    }
    try { this.onClose(reason, this); }
    finally { this.dispose(); }
  }

  dispose() {
    if (this.transfer) {
      const channel = this.transfer.channel;
      channel.onopen = channel.onmessage = channel.onclose = channel.onerror = null;
      this.transfer.stop();
      channel.close();
      this.transfer = null;
    }
    this.disposePeer();
  }
}

// Admits spectators from an already created watch room while the host
// machine runs, up to the places the room was created with.
export class SpectatorHub {
  constructor({ room, iceServers = [], relayOnly = false, slots, build, controller, media, machine,
    status = () => {}, changed = () => {}, PeerConnection }) {
    Object.assign(this, { room, iceServers, relayOnly, slots, build, controller, machine, status, changed, PeerConnection });
    this.snapshot = media;
    this.prepared = null;
    this.peers = new Map();
    this.closed = false;
    this.timer = null;
    this.polling = false;
  }

  // An ended room (expired, or closed with the game) has no invitation
  // worth showing; the spectators it admitted keep watching.
  get invitation() { return this.closed ? null : this.room.id ?? null; }

  // Hash the host media once, on the first spectator, not per spectator.
  media() {
    this.prepared ??= Promise.resolve(this.snapshot()).then(snapshot => describeMedia(snapshot));
    return this.prepared;
  }

  get watching() {
    let count = 0;
    for (const peer of this.peers.values()) if (peer.state === 'streaming') count++;
    return count;
  }

  start() {
    if (!this.closed && !this.polling) { this.polling = true; this.poll(); }
  }

  async poll() {
    if (this.closed) return;
    try {
      if (this.machine()) {
        const { offers } = await this.room.pollWatchOffers();
        for (const { spectator, code } of Array.isArray(offers) ? offers : []) {
          if (this.closed || this.peers.has(spectator)) continue;
          if (this.peers.size >= this.slots) {
            // The service only counts places still in signaling, so a
            // full house is told here rather than left waiting.
            try { await this.room.refuseWatch(spectator); }
            catch (error) { console.error('Spectator refusal failed', error); }
            continue;
          }
          const peer = new RtcWatchPeer({ token: spectator, iceServers: this.iceServers, PeerConnection: this.PeerConnection,
            build: this.build, controller: this.controller, media: () => this.media(), machine: this.machine,
            onClose: reason => {
              this.peers.delete(spectator);
              this.changed();
              if (!this.closed) this.status(`Spectator left: ${reason}`);
            } });
          this.peers.set(spectator, peer);
          try {
            peer.configureIce(this.iceServers, this.relayOnly);
            const answer = await peer.answer(code);
            if (peer.closed || this.closed) continue;
            await this.room.answerWatch(spectator, answer);
            this.changed();
          } catch (error) { peer.close(String(error.message ?? error)); }
        }
      }
    } catch (error) {
      if (/expired|ended/i.test(String(error.message))) {
        this.status('The spectator invitation ended; no more spectators can join.');
        this.close(false);
        return;
      }
      console.error('Spectator room poll failed', error);
    } finally {
      if (!this.closed) this.timer = setTimeout(() => this.poll(), POLL_MS);
    }
  }

  pump(emu) {
    const at = now();
    for (const peer of [...this.peers.values()]) peer.pump(emu, at);
  }

  close(dropPeers = true) {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.timer);
    this.timer = null;
    if (dropPeers) for (const peer of [...this.peers.values()]) peer.close('The host ended the game');
    this.room.end();
    // The invitation just went dead: the panel must stop showing it.
    this.changed();
  }
}
