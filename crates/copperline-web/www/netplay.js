// SPDX-License-Identifier: GPL-3.0-or-later

// Signaling is copy/paste; only bounded input packets use the data channel.
const CODE_LIMIT = 96 * 1024;
const PACKET_LIMIT = 943;
const QUEUE_LIMIT = 64;
const CHANNEL = 'copperline-netplay-v1';

export function validateSettings(value) {
  if (!value || !/^[0-9a-f]{32}$/i.test(value.session ?? '') ||
      !Number.isInteger(value.delay) || value.delay < 0 || value.delay > 6 ||
      !Number.isInteger(value.window) || value.window < 1 || value.window > 12 ||
      !['joystick', 'cd32'].includes(value.controller)) {
    throw new Error('Invalid netplay settings in connection code');
  }
  return { session: value.session.toLowerCase(), delay: value.delay,
    window: value.window, controller: value.controller };
}

export function encodeCode(description, settings) {
  const code = 'CLNP1.' + btoa(JSON.stringify({ description, settings: validateSettings(settings) }));
  if (code.length > CODE_LIMIT) throw new Error('Connection code is too large');
  return code;
}

export function decodeCode(code, type) {
  code = code.trim();
  if (code.length > CODE_LIMIT || !code.startsWith('CLNP1.')) {
    throw new Error('Paste a Copperline connection code');
  }
  let value;
  try { value = JSON.parse(atob(code.slice(6))); }
  catch { throw new Error('Connection code is incomplete or damaged'); }
  const description = value?.description;
  if (description?.type !== type || typeof description.sdp !== 'string' ||
      !description.sdp.startsWith('v=0\r\n') || description.sdp.length > CODE_LIMIT) {
    throw new Error(`Expected an ${type} connection code`);
  }
  return { description: { type, sdp: description.sdp }, settings: validateSettings(value.settings) };
}

export function newSettings(delay, window, controller) {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return validateSettings({ session: [...bytes].map(b => b.toString(16).padStart(2, '0')).join(''),
    delay, window, controller });
}

export class RtcLink {
  constructor({ iceServers = [], onOpen = () => {}, onClose = () => {},
    PeerConnection = globalThis.RTCPeerConnection } = {}) {
    if (!PeerConnection) throw new Error('This browser does not support WebRTC data channels');
    this.pc = new PeerConnection({ iceServers });
    this.channel = null;
    this.settings = null;
    this.incoming = [];
    this.closed = false;
    this.opened = false;
    this.onOpen = onOpen;
    this.onClose = onClose;
    this.timer = null;
    this.cancelGather = null;
    this.pc.ondatachannel = event => this.attach(event.channel);
    this.pc.onconnectionstatechange = () => {
      if (['failed', 'closed'].includes(this.pc.connectionState)) {
        this.close('Peer connection ended. Check the network and start a new session.');
      }
    };
  }

  attach(channel) {
    if (this.closed || this.channel || channel.label !== CHANNEL ||
        channel.ordered || channel.maxRetransmits !== 0) {
      channel.close();
      this.close('Unexpected netplay data channel');
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
      // newest packets, which repeat every unacknowledged input, as UDP does.
      if (this.incoming.length === QUEUE_LIMIT) this.incoming.shift();
      this.incoming.push(new Uint8Array(event.data));
    };
    channel.onopen = () => {
      if (this.closed || this.opened) return;
      this.opened = true;
      clearTimeout(this.timer);
      Promise.resolve().then(() => this.closed ? undefined : this.onOpen(this))
        .catch(error => this.close(String(error.message ?? error)));
    };
    channel.onclose = () => this.close('Peer disconnected');
    channel.onerror = () => this.close('Netplay data channel failed');
  }

  async gather(description) {
    if (this.closed) throw new Error('Connection cancelled');
    await this.pc.setLocalDescription(description);
    if (this.closed) throw new Error('Connection cancelled');
    if (this.pc.iceGatheringState !== 'complete') {
      await new Promise((resolve, reject) => {
        let timer;
        const finish = error => {
          clearTimeout(timer);
          this.pc.removeEventListener('icegatheringstatechange', changed);
          this.cancelGather = null;
          error ? reject(error) : resolve();
        };
        const changed = () => {
          if (this.pc.iceGatheringState === 'complete') finish();
        };
        this.cancelGather = () => finish(new Error('Connection cancelled'));
        this.pc.addEventListener('icegatheringstatechange', changed);
        timer = setTimeout(() => finish(new Error('Network address discovery timed out; check the STUN server')), 15000);
        changed();
      });
    }
    if (this.closed) throw new Error('Connection cancelled');
    return encodeCode(this.pc.localDescription, this.settings);
  }

  async offer(settings) {
    this.settings = validateSettings(settings);
    this.attach(this.pc.createDataChannel(CHANNEL, { ordered: false, maxRetransmits: 0 }));
    return this.gather(await this.pc.createOffer());
  }

  async answer(code) {
    const { description, settings } = decodeCode(code, 'offer');
    this.settings = settings;
    await this.pc.setRemoteDescription(description);
    if (this.closed) throw new Error('Connection cancelled');
    return this.gather(await this.pc.createAnswer());
  }

  async accept(code) {
    const { description, settings } = decodeCode(code, 'answer');
    if (JSON.stringify(settings) !== JSON.stringify(this.settings)) {
      throw new Error('Answer belongs to a different netplay session');
    }
    await this.pc.setRemoteDescription(description);
    if (this.closed) throw new Error('Connection cancelled');
    if (!this.opened) {
      clearTimeout(this.timer);
      this.timer = setTimeout(() => this.close('Peer connection timed out; try a LAN or VPN'), 60000);
    }
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
    clearTimeout(this.timer);
    this.cancelGather?.();
    this.incoming.length = 0;
    this.pc.ondatachannel = this.pc.onconnectionstatechange = null;
    if (this.channel) {
      this.channel.onopen = this.channel.onmessage = this.channel.onclose = this.channel.onerror = null;
      this.channel.close();
    }
    this.pc.close();
    this.onClose(reason, this);
  }
}

// The panel inserts itself into old static page shells, as the other controls do.
export function mountNetplayPanel(parent, { prepare, start, stop }) {
  const style = document.createElement('style');
  style.textContent = `
    #netplay-panel { font-size: .8rem; line-height: 1.4; color: var(--ink-mute, #bbc0ca); }
    #netplay-panel summary { cursor: pointer; font-weight: 600; color: var(--ink, #eee); }
    #netplay-panel p { margin: .6rem 0; }
    #netplay-panel label { display: block; margin-top: .6rem; }
    #netplay-panel input, #netplay-panel textarea, #netplay-panel select {
      display: block; box-sizing: border-box; width: 100%; margin-top: .2rem;
      border: 1px solid var(--line, #454b57); border-radius: 6px; padding: .35rem;
      background: rgba(10, 13, 22, .6); color: var(--ink, #eee); font: inherit;
    }
    #netplay-panel textarea { resize: vertical; font-family: ui-monospace, monospace; font-size: .7rem; }
    #netplay-panel .btn { margin-top: .4rem; width: 100%; justify-content: center; font-size: .8rem; padding: .35rem .5rem; }
    #netplay-panel :disabled { opacity: .45; cursor: default; }
    #netplay-panel #netplay-status { overflow-wrap: anywhere; color: var(--ink, #eee); }
  `;
  document.head.appendChild(style);
  const root = document.createElement('details');
  root.id = 'netplay-panel';
  root.className = 'try-side-section';
  root.innerHTML = `<summary>Netplay</summary>
    <p>Two browsers, matching ROMs and disks. Host is player 1; Join is player 2.</p>
    <label>Input delay <select id="netplay-delay">${[0,1,2,3,4,5,6].map(n => `<option ${n === 2 ? 'selected' : ''}>${n}</option>`).join('')}</select></label>
    <label>Rollback limit <select id="netplay-window">${Array.from({length:12}, (_, i) => `<option ${i === 7 ? 'selected' : ''}>${i + 1}</option>`).join('')}</select></label>
    <label>Controllers <select id="netplay-controller"><option value="joystick">Joystick</option><option value="cd32">CD32 pad</option></select></label>
    <label>STUN server <input id="netplay-stun" value="stun:stun.l.google.com:19302" spellcheck="false"></label>
    <p>STUN helps find a route over the internet. Leave it blank for LAN-only setup. Some networks need a VPN.</p>
    <button id="netplay-host" type="button">Host game</button>
    <label>Code from the other player <textarea id="netplay-remote" rows="3" spellcheck="false"></textarea></label>
    <button id="netplay-join" type="button">Join offer</button>
    <button id="netplay-accept" type="button" disabled>Connect answer</button>
    <label>Your connection code <textarea id="netplay-local" rows="3" readonly spellcheck="false"></textarea></label>
    <button id="netplay-copy" type="button" disabled>Copy code</button>
    <button id="netplay-disconnect" type="button" disabled>Disconnect</button>
    <p id="netplay-status" role="status">Load matching ROMs and disks, then host or join.</p>`;
  for (const button of root.querySelectorAll('button')) button.className = 'btn btn--ghost';
  root.addEventListener('keydown', event => event.stopPropagation());
  root.addEventListener('keyup', event => event.stopPropagation());
  parent.insertBefore(root, parent.querySelector('.try-side-section'));
  const field = name => root.querySelector(`#netplay-${name}`);
  let link = null;
  const status = text => { field('status').textContent = text; };
  const controls = () => {
    const active = !!link;
    for (const name of ['host', 'join', 'delay', 'window', 'controller', 'stun']) field(name).disabled = active;
    field('disconnect').disabled = !active;
    field('copy').disabled = !field('local').value;
    if (!active) field('accept').disabled = true;
  };
  async function begin(host) {
    if (link) return;
    let current;
    try {
      const remote = field('remote').value;
      const settings = host ? newSettings(Number(field('delay').value), Number(field('window').value), field('controller').value)
        : decodeCode(remote, 'offer').settings;
      const stun = field('stun').value.trim();
      if (stun && !/^stuns?:[^\s]+$/i.test(stun)) throw new Error('STUN server must start with stun: or stuns:');
      current = new RtcLink({ iceServers: stun ? [{ urls: stun }] : [],
        onOpen: async peer => {
          if (link !== peer) return;
          status('Connected. Checking the initial machines…');
          await start(peer, settings, host ? 1 : 2);
        },
        onClose: (reason, peer) => {
          if (link !== peer) return;
          link = null;
          field('local').value = '';
          controls();
          stop(reason);
          status(reason);
        },
      });
      link = current;
      field('local').value = '';
      controls();
      status('Preparing a fresh session and gathering network addresses…');
      await prepare(current);
      if (link !== current) return;
      const code = host ? await current.offer(settings) : await current.answer(remote);
      if (link !== current) return;
      field('local').value = code;
      field('copy').disabled = false;
      field('accept').disabled = !host;
      status(host ? 'Send your offer code. Paste the reply and click Connect answer.' : 'Send your answer code back to the host. Keep this page open.');
    } catch (error) {
      if (current && link === current) current.close(String(error.message ?? error));
      else if (!current) status(String(error.message ?? error));
    }
  }
  field('host').addEventListener('click', () => begin(true));
  field('join').addEventListener('click', () => begin(false));
  field('accept').addEventListener('click', async () => {
    const current = link;
    if (!current) return;
    field('accept').disabled = true;
    try {
      await current.accept(field('remote').value);
      if (link !== current) return;
      field('accept').disabled = true;
      if (!current.opened) status('Connecting to the other player…');
    } catch (error) {
      if (link !== current) return;
      field('accept').disabled = current.opened;
      status(String(error.message ?? error));
    }
  });
  field('copy').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText(field('local').value); status('Connection code copied'); }
    catch { field('local').focus(); field('local').select(); status('Copy the selected connection code'); }
  });
  field('disconnect').addEventListener('click', () => link?.close());
  window.addEventListener('pagehide', () => link?.close());
  return { get link() { return link; }, status, root };
}
