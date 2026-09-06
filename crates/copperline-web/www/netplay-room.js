// SPDX-License-Identifier: GPL-3.0-or-later
// Invitations contain an opaque room capability, never SDP or TURN credentials.
const ROOM = /^[A-Za-z0-9_-]{22}$/;
export function roomFromInvite(value) {
  value = value.trim();
  if (ROOM.test(value)) return value;
  try {
    const room = new URLSearchParams(new URL(value).hash.slice(1)).get('room');
    return ROOM.test(room ?? '') ? room : null;
  } catch { return null; }
}

export function inviteUrl(room, page = location.href) {
  if (!ROOM.test(room)) throw new Error('Invalid room');
  const url = new URL(page);
  // Do not accidentally share local media URLs or page configuration tokens.
  url.search = '';
  url.hash = new URLSearchParams({ room }).toString();
  return url.href;
}

export function signalingUrl(value) {
  const url = new URL(value);
  if (url.username || url.password || url.search || url.hash ||
      (url.protocol !== 'https:' && !(url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)))) {
    throw new Error('The room service must use HTTPS');
  }
  return url.href.replace(/\/$/, '');
}

export class RoomClient {
  constructor(base, signal) {
    this.base = signalingUrl(base);
    this.signal = signal;
    this.id = null;
    this.auth = null;
  }

  async request(path, method = 'GET', body, signal = this.signal) {
    const response = await fetch(`${this.base}${path}`, {
      method, signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(15000)]) : AbortSignal.timeout(15000),
      mode: 'cors', credentials: 'omit', cache: 'no-store', referrerPolicy: 'no-referrer',
      headers: { ...(body ? { 'Content-Type': 'application/json' } : {}),
        ...(this.auth ? { Authorization: `Bearer ${this.auth}` } : {}) },
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    if (Number(response.headers.get('Content-Length')) > 128 * 1024) throw new Error('Invalid room response');
    const reader = response.body?.getReader();
    if (!reader) throw new Error('Invalid room response');
    let text = '', size = 0;
    const decoder = new TextDecoder();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 128 * 1024) { await reader.cancel(); throw new Error('Invalid room response'); }
      text += decoder.decode(value, { stream: true });
    }
    const value = JSON.parse(text + decoder.decode());
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid room response');
    if (!response.ok) throw new Error(typeof value.error === 'string' ? value.error : 'Room request failed');
    return value;
  }

  async create() {
    const value = await this.request('/rooms', 'POST', {});
    if (!ROOM.test(value.id ?? '') || !ROOM.test(value.owner ?? '')) throw new Error('Invalid room response');
    this.id = value.id;
    this.auth = value.owner;
    return value;
  }

  async join(id) {
    if (!ROOM.test(id ?? '')) throw new Error('Paste an invitation link or room code');
    this.id = id;
    this.auth = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(16))))
      .replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');
    return this.request(`/rooms/${id}/join`, 'POST', { guest: this.auth });
  }

  publish(type, code) { return this.request(`/rooms/${this.id}/${type}`, 'POST', { code }); }

  async waitForAnswer(expiresAt) {
    while (!this.signal.aborted && Date.now() < expiresAt) {
      const { answer } = await this.request(`/rooms/${this.id}/answer`);
      if (typeof answer === 'string') return answer;
      await new Promise((resolve, reject) => {
        const done = () => { this.signal.removeEventListener('abort', cancel); resolve(); };
        const timer = setTimeout(done, 1500);
        const cancel = () => { clearTimeout(timer); this.signal.removeEventListener('abort', cancel); reject(this.signal.reason); };
        this.signal.addEventListener('abort', cancel, { once: true });
        if (this.signal.aborted) cancel();
      });
    }
    throw new Error('The invitation expired. Host a new game to try again.');
  }

  end() {
    if (!this.id || !this.auth) return Promise.resolve();
    return this.request(`/rooms/${this.id}`, 'DELETE', undefined, null).catch(() => {});
  }
}
