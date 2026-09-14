// SPDX-License-Identifier: GPL-3.0-or-later

import { RoomClient, inviteUrl, roomFromInvite, watchInviteUrl, watchFromInvite } from './netplay-room.js';
import qrcode from './netplay-qr.js';
import { MEDIA_CHANNEL, MEDIA_VERSION } from './netplay-media.js';
import { SWAP_CHANNEL, SWAP_VERSION, DISK_LIMIT, DiskSwaps } from './netplay-swap.js';
import { MediaTransfer } from './netplay-media.js';
import { RtcCommon, WATCH_VERSION, decodeCode, encodeCode, newSettings, validateSettings, validateWatchSettings } from './netplay-rtc.js';
import { RtcWatch, SpectatorHub } from './netplay-watch.js';

export { decodeCode, encodeCode, newSettings, validateSettings, validateWatchSettings };

// Signaling uses expiring room invitations or manual copy/paste codes.
// Only bounded input packets use the data channel.
export const PACKET_LIMIT = 1103;
const QUEUE_LIMIT = 64;
const CHANNEL = 'copperline-netplay-v1';
const SPECTATOR_SLOTS = 8;
const NOTICE_MS = 6000;

export class RtcLink extends RtcCommon {
  constructor({ iceServers = [], onOpen = () => {}, onClose = () => {},
    swapCallbacks = {},
    PeerConnection = globalThis.RTCPeerConnection } = {}) {
    super({ iceServers, PeerConnection });
    this.incoming = [];
    this.opened = false;
    this.onOpen = onOpen;
    this.onClose = onClose;
    this.media = null;
    this.swaps = null;
    this.swapCallbacks = swapCallbacks;
    this.mediaReady = new Promise((resolve, reject) => { this.mediaAttached = resolve; this.mediaFailed = reject; });
    this.mediaReady.catch(() => {});
    this.pc.ondatachannel = event => this.attach(event.channel);
  }

  attach(channel) {
    if (channel.label === SWAP_CHANNEL) {
      if (this.closed || this.swaps || this.settings?.swaps !== SWAP_VERSION ||
          channel.ordered !== true || channel.maxRetransmits != null || channel.maxPacketLifeTime != null) {
        channel.close();
        this.close('Unexpected disk swap channel. Refresh both pages.');
        return;
      }
      this.swaps = new DiskSwaps(channel, { ...this.swapCallbacks, host: this.host,
        fail: error => this.close(error.message) });
      return;
    }
    if (channel.label === MEDIA_CHANNEL) {
      if (this.closed || this.media || this.settings?.media !== MEDIA_VERSION ||
          channel.ordered !== true || channel.maxRetransmits != null || channel.maxPacketLifeTime != null) {
        channel.close();
        this.close('Unexpected game setup channel. Refresh both pages and start a new session.');
        return;
      }
      this.media = new MediaTransfer(channel, { host: this.host,
        fail: error => this.close(error.message) });
      this.mediaAttached(this.media);
      return;
    }
    if (this.closed || this.channel || channel.label !== CHANNEL ||
        channel.ordered || channel.maxRetransmits !== 0) {
      channel.close();
      this.close(`Unexpected netplay data channel: label=${channel.label}, ordered=${channel.ordered}, maxRetransmits=${channel.maxRetransmits}`);
      return;
    }
    this.channel = channel;
    channel.binaryType = 'arraybuffer';
    channel.onmessage = event => {
      if (this.closed) return;
      if (!(event.data instanceof ArrayBuffer) || event.data.byteLength > PACKET_LIMIT) {
        this.close('Invalid netplay packet');
        return;
      }
      // Browser timer throttling can batch valid retransmissions. Keep the
      // newest packets, which repeat every unacknowledged input.
      if (this.incoming.length === QUEUE_LIMIT) this.incoming.shift();
      this.incoming.push(new Uint8Array(event.data));
    };
    channel.onopen = () => {
      if (this.closed || this.opened) return;
      this.opened = true;
      this.diagnostics.record('data-open', this.pc);
      this.diagnostics.capture(this.pc);
      clearTimeout(this.timer);
      Promise.resolve().then(() => this.closed ? undefined : this.onOpen(this))
        .catch(error => { console.error('Netplay startup failed', error); this.close(String(error.message ?? error)); });
    };
    channel.onclose = () => {
      this.diagnostics.record('data-close', this.pc);
      this.close('Peer disconnected. Copy diagnostics for a connection report.');
    };
    channel.onerror = event => {
      this.diagnostics.record('data-error', this.pc);
      console.error('Netplay data channel failed', event.error ?? event);
      this.close(`Netplay data channel failed${event.error?.message ? ': ' + event.error.message : ''}`);
    };
  }

  async offer(settings) {
    this.settings = validateSettings(settings);
    this.host = true;
    this.attach(this.pc.createDataChannel(CHANNEL, { ordered: false, maxRetransmits: 0 }));
    if (this.settings.media === MEDIA_VERSION) this.attach(this.pc.createDataChannel(MEDIA_CHANNEL, { ordered: true }));
    if (this.settings.swaps === SWAP_VERSION) this.attach(this.pc.createDataChannel(SWAP_CHANNEL, { ordered: true }));
    return this.gather(await this.pc.createOffer());
  }

  async answer(code) {
    const { description, settings } = decodeCode(code, 'offer');
    if (settings.role) throw new Error('Expected a player offer, not a spectator code');
    this.settings = settings;
    this.host = false;
    await this.pc.setRemoteDescription(description);
    if (this.closed) throw new Error('Connection cancelled');
    return this.gather(await this.pc.createAnswer());
  }

  async accept(code) {
    const { description, settings } = decodeCode(code, 'answer');
    if (JSON.stringify(settings) !== JSON.stringify(this.settings)) {
      throw new Error('Answer belongs to a different netplay session or version. Refresh both pages and try again.');
    }
    await this.pc.setRemoteDescription(description);
    if (this.closed) throw new Error('Connection cancelled');
    if (!this.opened) {
      clearTimeout(this.timer);
      this.timer = setTimeout(() => this.close('Peer connection timed out. Copy diagnostics, then start a new session.'), 60000);
    }
  }

  async transferMedia(snapshot, progress) {
    if (this.closed) throw new Error('Game setup transfer cancelled');
    this.timer = setTimeout(() => this.close('Game setup transfer timed out. Start a new session.'), 180000);
    try {
      const transfer = await this.mediaReady;
      transfer.progress = progress;
      if (this.host) await transfer.send(snapshot);
      else return await transfer.receive();
    } finally { clearTimeout(this.timer); }
  }

  receive(emu) {
    for (const packet of this.incoming.splice(0)) emu.netplay_receive(packet);
  }

  send(emu) {
    if (this.closed || this.channel?.readyState !== 'open') return;
    for (let count = 0; count < QUEUE_LIMIT && this.channel.bufferedAmount < PACKET_LIMIT * QUEUE_LIMIT; count++) {
      const packet = emu.netplay_take_packet();
      if (!packet.length) break;
      this.channel.send(packet);
    }
  }

  close(reason = 'Disconnected. Start a new session to play again.') {
    if (this.closed) return;
    this.closed = true;
    this.diagnostics.record('stopped', this.pc);
    this.diagnostics.capture(this.pc);
    clearTimeout(this.timer);
    this.cancelGather?.();
    this.mediaFailed(new Error('Game setup transfer cancelled'));
    // Let the owner poll final queued packets for the core's failure reason
    // before freeing the machine. A remote close can follow its hello packet.
    try { this.onClose(reason, this); }
    finally { this.dispose(); }
  }

  dispose() {
    if (this.swaps) {
      const channel = this.swaps.channel;
      channel.onmessage = channel.onclose = channel.onerror = null;
      this.swaps.stop();
      channel.close();
      this.swaps = null;
    }
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

// The panel inserts itself into old static page shells, as the other controls do.
export function mountNetplayPanel(parent, { prepare, check = () => {}, start, stop, getMedia, useMedia, getMachine, diskChanged, build = () => '' }) {
  const style = document.createElement('style');
  style.textContent = `
    #netplay-panel { font-size: .88rem; line-height: 1.4; color: var(--ink-mute, #bbc0ca); }
    #netplay-panel summary { cursor: pointer; font-weight: 600; color: var(--ink, #eee); }
    #netplay-panel p { margin: .6rem 0; }
    #netplay-panel label { display: block; margin-top: .6rem; }
    #netplay-panel input, #netplay-panel textarea, #netplay-panel select {
      display: block; box-sizing: border-box; width: 100%; margin-top: .2rem;
      border: 1px solid var(--line, #454b57); border-radius: 6px; padding: .4rem;
      background: rgba(10, 13, 22, .6); color: var(--ink, #eee); font: inherit;
    }
    #netplay-panel textarea { resize: vertical; font-family: ui-monospace, monospace; font-size: .8rem; }
    #netplay-panel .btn { margin-top: .4rem; width: 100%; justify-content: center; font-size: .88rem; padding: .5rem; }
    #netplay-panel :disabled { opacity: .45; cursor: default; }
    #netplay-panel [hidden] { display: none !important; }
    #netplay-panel #netplay-status { overflow-wrap: anywhere; color: var(--ink, #eee); }
    #netplay-advanced { margin-top: .8rem; border-top: 1px solid var(--line, #454b57); padding-top: .6rem; }
    #netplay-qr, #netplay-watch-qr { margin: .8rem auto; max-width: 260px; background: white; padding: .25rem; }
    #netplay-qr svg, #netplay-watch-qr svg { display: block; width: 100%; height: auto; }
    #netplay-watch-invitation { margin-top: .8rem; border-top: 1px solid var(--line, #454b57); padding-top: .4rem; }
    #netplay-panel label:has(input[type=checkbox]) { display: flex; align-items: center; gap: .5rem; }
    #netplay-panel input[type=checkbox] { width: auto; margin: 0; }
    @media (pointer: coarse) {
      #netplay-panel input, #netplay-panel textarea, #netplay-panel select { font-size: 1rem; }
    }
  `;
  document.head.appendChild(style);
  const root = document.createElement('details');
  root.id = 'netplay-panel';
  root.className = 'try-side-section';
  root.innerHTML = `<summary>Netplay</summary>
    <p>The host shares their ROMs, disks and machine settings with player 2 and any spectators. Connecting replaces your running game; until player 2 arrives, the host can keep playing and change disks.</p>
    <div id="netplay-rooms">
      <button id="netplay-room-host" type="button">Host game</button>
      <label>Invitation link or room code <input id="netplay-room-code" autocomplete="off" autocapitalize="none" spellcheck="false"></label>
      <button id="netplay-room-join" type="button">Join game</button>
      <button id="netplay-room-watch" type="button" hidden>Watch game</button>
      <p id="netplay-service-status"></p>
    </div>
    <div id="netplay-invitation" hidden>
      <label>Your invitation <input id="netplay-invite" readonly spellcheck="false"></label>
      <button id="netplay-copy-invite" type="button">Copy invitation</button>
      <button id="netplay-share" type="button" hidden>Share invitation</button>
      <div id="netplay-qr" role="img" aria-label="Scan this QR code with the other device’s camera to join"></div>
      <p>Scan with the other device’s camera, or share the link. Invitations expire after 15 minutes.</p>
    </div>
    <div id="netplay-watch-invitation" hidden>
      <label>Spectator invitation <input id="netplay-watch-invite" readonly spellcheck="false"></label>
      <button id="netplay-copy-watch" type="button">Copy spectator invitation</button>
      <button id="netplay-share-watch" type="button" hidden>Share spectator invitation</button>
      <div id="netplay-watch-qr" role="img" aria-label="Scan this QR code with a spectator&rsquo;s camera to watch"></div>
      <p>Up to ${SPECTATOR_SLOTS} spectators can open this at any time while the game lasts. They receive your game files, replay the game from its start to catch up, and never send input.</p>
    </div>
    <button id="netplay-disconnect" type="button" disabled>Disconnect</button>
    <p id="netplay-status" role="status" aria-live="polite">Host a game or open an invitation to join.</p>
    <div id="netplay-disks" hidden>
      <p>The host can change disks here. Both players pause during the transfer and resume together.</p>
      <label>Drive <select id="netplay-disk-drive"><option value="0">DF0</option><option value="1">DF1</option></select></label>
      <label>Swap disk <input id="netplay-disk-file" type="file" accept=".adf,.adz,.dms,.ipf,.scp,.gz,.zip"></label>
      <label><input id="netplay-disk-writable" type="checkbox"> Allow writes to the replacement disk</label>
      <button id="netplay-disk-eject" type="button">Eject selected drive</button>
      <p>Writable changes stay in this session and are discarded when a disk is replaced or the session ends.</p>
    </div>
    <button id="netplay-diagnostics" type="button" disabled>Copy diagnostics</button>
    <textarea id="netplay-report" rows="5" readonly hidden aria-label="Connection diagnostics"></textarea>
    <details id="netplay-advanced"><summary>Advanced</summary>
      <label>Controllers <select id="netplay-controller"><option value="joystick">Joystick</option><option value="cd32">CD32 pad</option><option value="mouse">Two mice</option></select></label>
      <p>For two-mouse games, choose Two mice before hosting. Each player’s mouse or touch trackpad controls their own Amiga port.</p>
      <label>Input delay <select id="netplay-delay">${[0,1,2,3,4,5,6].map(n => `<option ${n === 2 ? 'selected' : ''}>${n}</option>`).join('')}</select></label>
      <label>Rollback limit <select id="netplay-window">${Array.from({length:12}, (_, i) => `<option ${i === 7 ? 'selected' : ''}>${i + 1}</option>`).join('')}</select></label>
      <label><input id="netplay-relay-only" type="checkbox"> Use relay only for room connections</label>
      <p>Room connections try a direct route and can use a relay automatically. Relay-only mode helps diagnose connection problems.</p>
      <h4>Manual connection codes</h4>
      <label>STUN server <input id="netplay-stun" value="stun:stun.l.google.com:19302" spellcheck="false"></label>
      <p>Leave STUN blank for LAN-only discovery. Manual setup has no relay fallback.</p>
      <button id="netplay-host" type="button">Host with manual codes</button>
      <label>Code from the other player <textarea id="netplay-remote" rows="3" spellcheck="false"></textarea></label>
      <button id="netplay-join" type="button">Join offer</button>
      <button id="netplay-accept" type="button" disabled>Connect answer</button>
      <label>Your connection code <textarea id="netplay-local" rows="3" readonly spellcheck="false"></textarea></label>
      <button id="netplay-copy" type="button" disabled>Copy code</button>
    </details>`;
  for (const button of root.querySelectorAll('button')) button.className = 'btn btn--ghost';
  root.addEventListener('keydown', event => event.stopPropagation());
  root.addEventListener('keyup', event => event.stopPropagation());
  parent.insertBefore(root, parent.querySelector('.try-side-section'));
  const field = name => root.querySelector(`#netplay-${name}`);
  const service = document.querySelector('meta[name="copperline-netplay-service"]')?.content?.trim();
  let link = null;
  let lastLink = null;
  let readingDisk = false;
  const status = text => { field('status').textContent = text; };
  // A notice outlives the once-a-second frame report for a moment, so a
  // spectator change or departure is readable rather than overwritten.
  let noticeUntil = 0;
  const notice = text => { status(text); noticeUntil = Date.now() + NOTICE_MS; };
  const controls = () => {
    const active = !!link;
    for (const name of ['host', 'join', 'delay', 'window', 'controller', 'stun', 'relay-only', 'room-code']) field(name).disabled = active;
    for (const name of ['room-host', 'room-join', 'room-watch']) field(name).disabled = active || !service;
    field('disconnect').disabled = !active;
    field('copy').disabled = !field('local').value;
    field('diagnostics').disabled = !lastLink;
    // The player invitation is spent once player 2 is on the line; from
    // then on only the spectator invitation is worth showing.
    field('invitation').hidden = !field('invite').value || !!link?.opened;
    field('watch-invitation').hidden = !link?.hub?.invitation;
    const swapEnabled = !!link?.host && link.settings?.swaps === SWAP_VERSION;
    field('disks').hidden = !swapEnabled;
    const canSwap = swapEnabled && link.swaps?.channel.readyState === 'open'
      && !!getMachine?.(link)?.netplay_status()[0] && !link.swaps.busy && !readingDisk;
    for (const name of ['disk-drive', 'disk-file', 'disk-writable', 'disk-eject']) field(name).disabled = !canSwap;
    if (!active) field('accept').disabled = true;
  };
  field('service-status').textContent = service
    ? 'Host is player 1; Join is player 2; Watch follows a game as a spectator. Received files are used only for this session.'
    : 'Room invitations are not configured on this page. Manual setup is available under Advanced.';
  field('advanced').open = !service;
  field('share').hidden = field('share-watch').hidden = typeof navigator.share !== 'function';

  function readInvitation() {
    if (link) return;
    const params = new URLSearchParams(location.hash.slice(1));
    const room = params.get('room');
    const watch = params.get('watch');
    if (!room && !watch) return;
    root.open = true;
    if (watch) {
      if (!watchFromInvite(watch)) { status('This spectator invitation is incomplete or damaged.'); return; }
      field('room-code').value = location.href;
      offerButtons();
      status('Spectator invitation ready. Click Watch game to receive the host’s files and follow the game.');
      return;
    }
    if (!roomFromInvite(room)) { status('This invitation is incomplete or damaged.'); return; }
    field('room-code').value = room;
    offerButtons();
    status('Invitation ready. Click Join game to receive the host’s files and machine settings.');
  }
  // A spectator link can only be watched and a player invitation only
  // joined, so the code field offers the one button that fits it.
  function offerButtons() {
    const watching = /[#&?]watch=/.test(field('room-code').value);
    field('room-watch').hidden = !watching;
    field('room-join').hidden = watching;
  }
  readInvitation();
  window.addEventListener('hashchange', readInvitation);
  field('room-code').addEventListener('input', offerButtons);

  async function begin(mode) {
    if (link) return;
    const host = mode.endsWith('host');
    const roomMode = mode.startsWith('room-');
    const watch = mode === 'room-watch';
    let current;
    let settings;
    try {
      const remote = field('remote').value;
      const roomId = watch ? watchFromInvite(field('room-code').value) : roomFromInvite(field('room-code').value);
      if (roomMode && !service) throw new Error('Room invitations are not configured on this page');
      if (roomMode && !host && !roomId) throw new Error(watch ? 'Paste a spectator invitation link' : 'Paste an invitation link or room code');
      settings = host ? { ...newSettings(Number(field('delay').value), Number(field('window').value), field('controller').value), media: MEDIA_VERSION, swaps: SWAP_VERSION }
        : roomMode ? null : decodeCode(remote, 'offer').settings;
      const stun = field('stun').value.trim();
      if (!roomMode && stun && !/^stuns?:[^\s]+$/i.test(stun)) throw new Error('STUN server must start with stun: or stuns:');
      const callbacks = {
        onOpen: async peer => {
          if (link !== peer) return;
          controls();
          if (host) {
            // The host's page stayed live while it waited; its media and
            // settings are captured now that player 2 is on the line.
            status('Preparing a fresh session...');
            await prepare(peer, { host, receiveMedia: false });
            if (link !== peer) return;
          }
          if (watch) {
            status('Receiving the host’s game setup...');
            const received = await peer.transferMedia(null, (action, bytes, total) => {
              if (link === peer) status(`${action} game setup: ${Math.floor(bytes * 100 / total)}%`);
            });
            if (link !== peer) return;
            useMedia(peer, received);
            status('Connected. Checking the initial machine...');
            await start(peer, peer.settings, 'watch');
            return;
          }
          if (settings.media === MEDIA_VERSION) {
            status(host ? 'Sending game setup...' : 'Receiving the host’s game setup...');
            const received = await peer.transferMedia(host ? getMedia(peer) : null, (action, bytes, total) => {
              if (link === peer) status(`${action} game setup: ${Math.floor(bytes * 100 / total)}%`);
            });
            if (link !== peer) return;
            if (!host) useMedia(peer, received);
          }
          status('Connected. Checking the initial machines...');
          await start(peer, settings, host ? 1 : 2);
        },
        onClose: (reason, peer) => {
          if (link !== peer) return;
          peer.abort.abort();
          peer.hub?.close();
          peer.room?.end();
          link = null;
          field('local').value = '';
          field('invite').value = '';
          field('watch-invite').value = '';
          field('qr').replaceChildren();
          field('watch-qr').replaceChildren();
          controls();
          status(stop(reason, peer) ?? reason);
        },
      };
      current = watch ? new RtcWatch(callbacks) : new RtcLink({ iceServers: !roomMode && stun ? [{ urls: stun }] : [],
        swapCallbacks: {
          machine: () => getMachine(current), status,
          changed: disk => { if (link === current) { if (disk) diskChanged(current, disk); controls(); } },
        },
        ...callbacks,
      });
      current.abort = new AbortController();
      link = lastLink = current;
      field('local').value = '';
      field('report').hidden = true;
      controls();
      if (host) check();
      else {
        status(watch ? 'Preparing to watch...' : 'Preparing a fresh session...');
        await prepare(current, { host, receiveMedia: roomMode || settings?.media === MEDIA_VERSION });
        if (link !== current) return;
      }
      if (watch) {
        current.room = new RoomClient(service, current.abort.signal, '/watch');
        status('Joining as a spectator...');
        const network = await current.room.join(roomId);
        if (link !== current) { current.room.end(); return; }
        if (!Number.isFinite(network.expiresAt) || network.expiresAt <= Date.now()) throw new Error('The spectator invitation has expired');
        current.configureIce(network.iceServers, field('relay-only').checked);
        status('Finding a connection route...');
        const code = await current.offer({ role: 'watch', build: build(), media: MEDIA_VERSION, watch: WATCH_VERSION });
        if (link !== current) return;
        await current.room.publish('offer', code);
        if (link !== current) return;
        status('Waiting for the host to admit you...');
        const answer = await current.room.waitForAnswer(network.expiresAt);
        if (link !== current) return;
        await current.accept(answer);
        if (link === current && !current.opened) status('Connecting to the host...');
      } else if (roomMode) {
        current.room = new RoomClient(service, current.abort.signal);
        status(host ? 'Creating your invitation...' : 'Joining the room...');
        const network = host ? await current.room.create() : await current.room.join(roomId);
        if (link !== current) { current.room.end(); return; }
        if (!Number.isFinite(network.expiresAt) || network.expiresAt <= Date.now()) throw new Error('The invitation has expired');
        if (!host) settings = decodeCode(network.offer, 'offer').settings;
        if (host) {
          // A separate room, with its own capability, admits spectators for
          // as long as the host keeps polling it. Every hosted game offers
          // the full number of places; the machine keeps its history from
          // frame zero so a spectator can arrive at any time.
          current.watch = new RoomClient(service, current.abort.signal, '/watch');
          const watchRoom = await current.watch.create({ slots: SPECTATOR_SLOTS });
          if (link !== current) { current.watch.end(); current.room.end(); return; }
          current.hub = new SpectatorHub({ room: current.watch, iceServers: watchRoom.iceServers,
            relayOnly: field('relay-only').checked, slots: SPECTATOR_SLOTS, build: build(), controller: settings.controller,
            media: () => getMedia(current), machine: () => getMachine(current),
            status: text => { if (link === current) notice(text); }, changed: controls });
        }
        current.configureIce(network.iceServers, field('relay-only').checked);
        status('Finding a connection route...');
        const code = host ? await current.offer(settings) : await current.answer(network.offer);
        if (link !== current) return;
        await current.room.publish(host ? 'offer' : 'answer', code);
        if (link !== current) return;
        if (host) {
          const invitation = inviteUrl(current.room.id);
          field('invite').value = invitation;
          field('qr').innerHTML = qrSvg(invitation);
          const watchInvitation = watchInviteUrl(current.watch.id);
          field('watch-invite').value = watchInvitation;
          field('watch-qr').innerHTML = qrSvg(watchInvitation);
          controls();
          status('Waiting for player 2. Share the invitation or scan the QR code.');
          const answer = await current.room.waitForAnswer(network.expiresAt);
          if (link !== current) return;
          await current.accept(answer);
        } else if (!current.opened) {
          current.timer = setTimeout(() => current.close('Connection timed out. Copy diagnostics, then start a new room.'), 60000);
        }
        if (link === current && !current.opened) status('Connecting to the other player...');
      } else {
        status('Gathering network addresses...');
        const code = host ? await current.offer(settings) : await current.answer(remote);
        if (link !== current) return;
        field('local').value = code;
        field('copy').disabled = false;
        field('accept').disabled = !host;
        status(host ? 'Send your offer code. Paste the reply and click Connect answer.' : 'Send your answer code back to the host. Keep this page open, or Disconnect to cancel.');
      }
    } catch (error) {
      if (current && link === current) current.close(String(error.message ?? error));
      else if (!current) status(String(error.message ?? error));
    }
  }
  function qrSvg(text) {
    const qr = qrcode(0, 'M');
    qr.addData(text);
    qr.make();
    return qr.createSvgTag({ cellSize: 4, margin: 16, scalable: true });
  }
  field('host').addEventListener('click', () => begin('manual-host'));
  field('join').addEventListener('click', () => begin('manual-join'));
  field('room-host').addEventListener('click', () => begin('room-host'));
  field('room-join').addEventListener('click', () => begin('room-join'));
  field('room-watch').addEventListener('click', () => begin('room-watch'));
  field('disk-file').addEventListener('change', async () => {
    const current = link;
    const file = field('disk-file').files?.[0];
    if (!file || !current?.host || !current.swaps || readingDisk) return;
    const drive = Number(field('disk-drive').value);
    const writable = field('disk-writable').checked;
    readingDisk = true;
    controls();
    try {
      if (!file.size || file.size > DISK_LIMIT) throw new Error('Select a disk image of up to 16 MiB');
      const bytes = new Uint8Array(await file.arrayBuffer());
      if (link !== current) return;
      await current.swaps.swap(drive, { bytes, name: file.name, writable });
    } catch (error) { if (link === current) status(String(error.message ?? error)); }
    finally { readingDisk = false; field('disk-file').value = ''; controls(); }
  });
  field('disk-eject').addEventListener('click', async () => {
    const current = link;
    if (!current?.host || !current.swaps || readingDisk) return;
    try { await current.swaps.swap(Number(field('disk-drive').value), null); }
    catch (error) { if (link === current) status(String(error.message ?? error)); }
  });
  field('accept').addEventListener('click', async () => {
    const current = link;
    if (!current) return;
    field('accept').disabled = true;
    try {
      await current.accept(field('remote').value);
      if (link !== current) return;
      if (!current.opened) status('Connecting to the other player...');
    } catch (error) {
      if (link !== current) return;
      field('accept').disabled = current.opened;
      status(String(error.message ?? error));
    }
  });
  async function copy(name, message) {
    try { await navigator.clipboard.writeText(field(name).value); status(message); }
    catch { field(name).hidden = false; field(name).focus(); field(name).select(); status('Copy the selected text'); }
  }
  field('copy').addEventListener('click', () => copy('local', 'Connection code copied'));
  field('copy-invite').addEventListener('click', () => copy('invite', 'Invitation copied'));
  field('copy-watch').addEventListener('click', () => copy('watch-invite', 'Spectator invitation copied'));
  field('share').addEventListener('click', async () => {
    try { await navigator.share({ title: 'Join my Copperline game', url: field('invite').value }); }
    catch (error) { if (error.name !== 'AbortError') copy('invite', 'Invitation copied'); }
  });
  field('share-watch').addEventListener('click', async () => {
    try { await navigator.share({ title: 'Watch my Copperline game', url: field('watch-invite').value }); }
    catch (error) { if (error.name !== 'AbortError') copy('watch-invite', 'Spectator invitation copied'); }
  });
  field('diagnostics').addEventListener('click', async () => {
    const peer = lastLink;
    if (!peer) return;
    field('report').value = JSON.stringify(await peer.report(), null, 2);
    await copy('report', 'Diagnostics copied. The report excludes connection codes, credentials and network addresses.');
  });
  field('disconnect').addEventListener('click', () => link?.close());
  window.addEventListener('pagehide', () => link?.close());
  controls();
  return { get link() { return link; }, status: text => { controls(); if (!link?.swaps?.busy && Date.now() >= noticeUntil) status(text); }, root };
}
