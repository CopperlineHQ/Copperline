// SPDX-License-Identifier: GPL-3.0-or-later
// Connection codes and the WebRTC plumbing shared by player and spectator links.
import { NetplayDiagnostics, connectionFailure } from './netplay-diagnostics.js';
import { MEDIA_VERSION } from './netplay-media.js';
import { SWAP_VERSION } from './netplay-swap.js';

export const CODE_LIMIT = 96 * 1024;
export const WATCH_VERSION = 'watch-v1';
export const WATCH_CHANNEL = 'copperline-watch-v1';
const CONTROLLERS = ['joystick', 'cd32', 'mouse'];

export function validateSettings(value) {
  if (!value || !/^[0-9a-f]{32}$/i.test(value.session ?? '') ||
      !Number.isInteger(value.delay) || value.delay < 0 || value.delay > 6 ||
      !Number.isInteger(value.window) || value.window < 1 || value.window > 12 ||
      !CONTROLLERS.includes(value.controller) ||
      (value.media !== undefined && value.media !== MEDIA_VERSION) ||
      (value.swaps !== undefined && value.swaps !== SWAP_VERSION)) {
    throw new Error('Invalid netplay settings in connection code');
  }
  return { session: value.session.toLowerCase(), delay: value.delay,
    window: value.window, controller: value.controller,
    ...(value.media ? { media: value.media } : {}), ...(value.swaps ? { swaps: value.swaps } : {}) };
}

// A spectator's offer names its build and channel versions; the host's
// answer adds the controller both players use, which the spectator's
// machine must fit before it can reproduce the host's fingerprint.
export function validateWatchSettings(value) {
  if (!value || value.role !== 'watch' ||
      typeof value.build !== 'string' || !value.build.length || value.build.length > 128 ||
      value.media !== MEDIA_VERSION || value.watch !== WATCH_VERSION ||
      (value.controller !== undefined && !CONTROLLERS.includes(value.controller))) {
    throw new Error('Invalid spectator settings in connection code');
  }
  return { role: 'watch', build: value.build, media: MEDIA_VERSION, watch: WATCH_VERSION,
    ...(value.controller ? { controller: value.controller } : {}) };
}

function validateAnySettings(value) {
  return value?.role === 'watch' ? validateWatchSettings(value) : validateSettings(value);
}

export function encodeCode(description, settings) {
  const code = 'CLNP1.' + btoa(JSON.stringify({ description, settings: validateAnySettings(settings) }));
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
  return { description: { type, sdp: description.sdp }, settings: validateAnySettings(value.settings) };
}

export function newSettings(delay, window, controller) {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return validateSettings({ session: [...bytes].map(b => b.toString(16).padStart(2, '0')).join(''),
    delay, window, controller });
}

// One RTCPeerConnection with address gathering, ICE configuration and
// diagnostics. Subclasses own their channels and implement close(reason).
export class RtcCommon {
  constructor({ iceServers = [], PeerConnection = globalThis.RTCPeerConnection } = {}) {
    if (!PeerConnection) throw new Error('This browser does not support WebRTC data channels');
    this.pc = new PeerConnection({ iceServers });
    this.channel = null;
    this.settings = null;
    this.closed = false;
    this.timer = null;
    this.cancelGather = null;
    this.diagnostics = new NetplayDiagnostics();
    this.diagnostics.record('created', this.pc);
    this.pc.onconnectionstatechange = () => {
      this.diagnostics.record('peer-state', this.pc);
      this.diagnostics.capture(this.pc);
      if (['failed', 'closed'].includes(this.pc.connectionState)) {
        this.close(connectionFailure(this.pc));
      }
    };
    this.pc.oniceconnectionstatechange = () => {
      this.diagnostics.record('ice-state', this.pc);
      this.diagnostics.capture(this.pc);
    };
    this.pc.onicegatheringstatechange = () => this.diagnostics.record('gathering-state', this.pc);
    this.pc.onsignalingstatechange = () => this.diagnostics.record('signaling-state', this.pc);
    this.pc.onicecandidateerror = event => {
      this.diagnostics.iceError(event.errorCode);
      this.diagnostics.record('ice-error', this.pc);
    };
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
        timer = setTimeout(() => {
          // One slow/unreachable ICE server must not discard usable routes
          // from the others. Later candidates may be omitted from this offer.
          if (/^a=candidate:/m.test(this.pc.localDescription?.sdp ?? '')) {
            this.diagnostics.record('gathering-deadline', this.pc);
            finish();
          } else finish(new Error('Network address discovery timed out without a usable route. Copy diagnostics, then try a new session.'));
        }, 15000);
        changed();
      });
    }
    if (this.closed) throw new Error('Connection cancelled');
    return encodeCode(this.pc.localDescription, this.settings);
  }

  configureIce(iceServers, relayOnly = false) {
    if (!Array.isArray(iceServers) || iceServers.length > 8) throw new Error('Invalid network configuration');
    if (relayOnly && !iceServers.some(server => [].concat(server.urls ?? []).some(url => /^turns?:/.test(url)))) {
      throw new Error('A relay is not available for this session');
    }
    const iceTransportPolicy = relayOnly ? 'relay' : 'all';
    try { this.pc.setConfiguration({ iceServers, iceTransportPolicy }); }
    catch (error) {
      if (error.name !== 'SyntaxError') throw error;
      // Some WebKit builds reject valid TURN transport queries. Retry using
      // default UDP for turn: and TLS/TCP for turns:, retaining ports and keys.
      // Plain TCP needs its query, so omit it only on this compatibility path.
      const compatible = iceServers.map(server => ({ ...server,
        urls: [].concat(server.urls ?? []).flatMap(url => {
          if (/^turn:[^?]+\?transport=udp$/i.test(url) || /^turns:[^?]+\?transport=tcp$/i.test(url)) return [url.split('?')[0]];
          if (/^turn:[^?]+\?transport=tcp$/i.test(url)) return [];
          return [url];
        }),
      })).filter(server => server.urls.length);
      if (JSON.stringify(compatible) === JSON.stringify(iceServers) ||
          !compatible.some(server => server.urls.some(url => /^turns?:/.test(url)))) throw error;
      this.pc.setConfiguration({ iceServers: compatible, iceTransportPolicy });
    }
  }

  report() { return this.diagnostics.report(this.pc, this.channel); }

  // Drop every handler before closing so a late browser event cannot run
  // against a disposed link.
  disposePeer() {
    this.pc.ondatachannel = this.pc.onconnectionstatechange = null;
    this.pc.oniceconnectionstatechange = this.pc.onicegatheringstatechange = null;
    this.pc.onsignalingstatechange = this.pc.onicecandidateerror = null;
    if (this.channel) {
      this.channel.onopen = this.channel.onmessage = this.channel.onclose = this.channel.onerror = null;
      this.channel.close();
    }
    this.pc.close();
  }
}
