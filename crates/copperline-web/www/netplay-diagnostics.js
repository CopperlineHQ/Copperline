// SPDX-License-Identifier: GPL-3.0-or-later
// Export only enumerated states and numeric counters. SDP, candidate addresses,
// invitation/session tokens, TURN credentials and arbitrary error text stay out.
const states = new Set(['new', 'checking', 'connected', 'completed', 'disconnected', 'failed', 'closed',
  'gathering', 'complete', 'stable', 'have-local-offer', 'have-remote-offer', 'have-local-pranswer',
  'have-remote-pranswer', 'connecting', 'open', 'closing', 'waiting', 'in-progress', 'succeeded', 'frozen']);
const types = new Set(['host', 'srflx', 'prflx', 'relay']);
const state = value => states.has(value) ? value : 'unknown';
const number = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
const candidateType = value => types.has(value) ? value : 'unknown';

export function candidateCounts(sdp = '') {
  const counts = { host: 0, srflx: 0, prflx: 0, relay: 0 };
  for (const line of sdp.split('\n')) {
    if (!line.startsWith('a=candidate:')) continue;
    const type = /\btyp (host|srflx|prflx|relay)\b/.exec(line)?.[1];
    if (type) counts[type]++;
  }
  return counts;
}

export function summarizeStats(stats) {
  const values = [...stats.values()];
  const transport = values.find(value => value.type === 'transport');
  const pair = stats.get(transport?.selectedCandidatePairId)
    ?? values.find(value => value.type === 'candidate-pair' && value.nominated && value.state === 'succeeded');
  const local = stats.get(pair?.localCandidateId);
  const remote = stats.get(pair?.remoteCandidateId);
  return {
    dtls: state(transport?.dtlsState),
    selectedPair: pair ? {
      state: state(pair.state), local: candidateType(local?.candidateType), remote: candidateType(remote?.candidateType),
      protocol: ['udp', 'tcp'].includes(local?.protocol) ? local.protocol : 'unknown',
      roundTripSeconds: number(pair.currentRoundTripTime),
      bytesSent: number(pair.bytesSent), bytesReceived: number(pair.bytesReceived),
    } : null,
    pairs: values.filter(value => value.type === 'candidate-pair').slice(0, 32).map(value => ({
      state: state(value.state), nominated: value.nominated === true,
      requestsSent: number(value.requestsSent), responsesReceived: number(value.responsesReceived),
    })),
  };
}

export class NetplayDiagnostics {
  constructor() {
    this.started = performance.now();
    this.events = [];
    this.connection = null;
    this.stats = null;
    this.pending = Promise.resolve();
    this.iceErrors = [];
    this.sample = 0;
  }

  record(event, pc) {
    this.connection = {
      peer: state(pc.connectionState), ice: state(pc.iceConnectionState),
      gathering: state(pc.iceGatheringState), signaling: state(pc.signalingState),
      localCandidates: candidateCounts(pc.localDescription?.sdp),
      remoteCandidates: candidateCounts(pc.remoteDescription?.sdp),
    };
    // Call sites supply fixed event names. Do not accept arbitrary reason strings.
    const kinds = ['created', 'peer-state', 'ice-state', 'gathering-state', 'signaling-state',
      'data-open', 'data-close', 'data-error', 'ice-error', 'gathering-deadline', 'stopped', 'copy'];
    if (kinds.includes(event)) this.events.push({ event, elapsedMs: Math.round(performance.now() - this.started),
      peer: this.connection.peer, ice: this.connection.ice });
    if (this.events.length > 40) this.events.shift();
  }

  capture(pc) {
    if (typeof pc.getStats !== 'function' || pc.connectionState === 'closed') return this.pending;
    // Invoke before dispose closes the connection; retain the last useful stats
    // if the browser rejects a final sample during teardown.
    const sample = ++this.sample;
    try {
      this.pending = Promise.resolve(pc.getStats()).then(stats => {
        if (sample === this.sample) this.stats = summarizeStats(stats);
      }).catch(() => {});
    } catch { /* Some browsers throw synchronously during teardown. */ }
    return this.pending;
  }

  iceError(code) {
    if (Number.isInteger(code) && code >= 0 && code <= 999 && this.iceErrors.length < 8) this.iceErrors.push(code);
  }

  async report(pc, channel) {
    if (pc.connectionState !== 'closed') { this.record('copy', pc); await this.capture(pc); }
    await this.pending;
    return {
      format: 'copperline-netplay-diagnostics-v1',
      browser: typeof navigator === 'undefined' ? 'test' : navigator.userAgent.slice(0, 256),
      secureContext: globalThis.isSecureContext === true,
      connection: this.connection,
      channel: channel ? { state: state(channel.readyState), ordered: channel.ordered === true,
        maxRetransmits: number(channel.maxRetransmits), bufferedAmount: number(channel.bufferedAmount) } : null,
      stats: this.stats, iceErrorCodes: this.iceErrors.slice(), events: this.events.slice(),
    };
  }
}

export function connectionFailure(pc) {
  const ice = state(pc.iceConnectionState);
  const peer = state(pc.connectionState);
  const stage = ice === 'failed' ? 'Network route discovery failed (ICE)'
    : ['connected', 'completed'].includes(ice) ? 'The network route opened, but the peer transport ended'
    : 'The peer connection ended during setup';
  return `${stage}. ICE: ${ice}; connection: ${peer}. Copy diagnostics for a connection report.`;
}
