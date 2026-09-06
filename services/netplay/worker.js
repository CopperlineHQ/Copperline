// SPDX-License-Identifier: GPL-3.0-or-later
import { DurableObject } from 'cloudflare:workers';

const ROOM_TTL = 15 * 60 * 1000;
const BODY_LIMIT = 100 * 1024;
const TOKEN = /^[A-Za-z0-9_-]{22}$/;
const turnUrl = id => `https://rtc.live.cloudflare.com/v1/turn/keys/${encodeURIComponent(id)}/credentials/generate-ice-servers`;
const json = (body, status = 200) => Response.json(body, { status, headers: { 'Cache-Control': 'no-store' } });
const token = () => btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(16))))
  .replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');

class RequestError extends Error {}

async function readJson(request) {
  if (!request.headers.get('Content-Type')?.startsWith('application/json')) throw new RequestError('Expected JSON');
  const reader = request.body?.getReader();
  if (!reader) throw new RequestError('Expected JSON');
  const chunks = [];
  let size = 0;
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    size += value.length;
    if (size > BODY_LIMIT) { await reader.cancel(); throw new RequestError('Request too large'); }
    chunks.push(value);
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  let value;
  try { value = JSON.parse(new TextDecoder().decode(bytes)); }
  catch { throw new RequestError('Invalid JSON'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new RequestError('Expected JSON object');
  return value;
}

function connectionCode(code, type) {
  if (typeof code !== 'string' || code.length > 96 * 1024 || !code.startsWith('CLNP1.')) return false;
  try {
    const value = JSON.parse(atob(code.slice(6)));
    return value?.description?.type === type && typeof value.description.sdp === 'string'
      && value.description.sdp.startsWith('v=0\r\n');
  } catch { return false; }
}

async function iceServers(env) {
  if (!env.TURN_KEY_ID || !env.TURN_KEY_API_TOKEN) {
    if (env.REQUIRE_TURN !== 'false') throw new Error('Relay service is not configured');
    // Explicit local-development mode: no third-party network requests.
    return { iceServers: [], relay: false };
  }
  const response = await fetch(turnUrl(env.TURN_KEY_ID), {
    method: 'POST',
    headers: { Authorization: `Bearer ${env.TURN_KEY_API_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ ttl: 86400 }),
    signal: AbortSignal.timeout(10000),
  });
  if (!response.ok) throw new Error('Relay credentials are unavailable');
  const value = await response.json();
  if (!Array.isArray(value.iceServers) || !value.iceServers.some(server =>
    [].concat(server.urls ?? []).some(url => /^turns?:/.test(url)))) throw new Error('Invalid relay response');
  return { iceServers: value.iceServers, relay: true };
}

export default {
  async fetch(request, env) {
    const origin = request.headers.get('Origin');
    const allowed = (env.ALLOWED_ORIGINS ?? '').split(',').map(value => value.trim());
    if (!origin || !allowed.includes(origin)) return json({ error: 'Origin not allowed' }, 403);
    const cors = {
      'Access-Control-Allow-Origin': origin,
      'Access-Control-Allow-Methods': 'GET, POST, DELETE, OPTIONS',
      'Access-Control-Allow-Headers': 'Authorization, Content-Type',
      'Access-Control-Max-Age': '600',
      'Cache-Control': 'no-store',
      Vary: 'Origin',
    };
    if (request.method === 'OPTIONS') return new Response(null, { status: 204, headers: cors });
    let response;
    try {
      const path = new URL(request.url).pathname;
      const match = /^\/rooms\/([A-Za-z0-9_-]{22})(?:\/(offer|join|answer))?$/.exec(path);
      if (path === '/health' && request.method === 'GET') {
        response = json({ service: 'copperline-netplay', version: 1,
          relay: !!(env.TURN_KEY_ID && env.TURN_KEY_API_TOKEN) });
      } else if ((path === '/rooms' && request.method === 'POST') || match) {
        // Bound both creation and room traffic. Keys are never persisted or logged.
        const key = request.headers.get('CF-Connecting-IP') ?? 'local';
        if (!env.ROOM_RATE_LIMIT || !(await env.ROOM_RATE_LIMIT.limit({ key })).success ||
            (!match && (!env.ROOM_CREATE_LIMIT || !(await env.ROOM_CREATE_LIMIT.limit({ key })).success))) {
          response = json({ error: 'Too many requests. Wait a minute and try again.' }, 429);
        } else {
          const id = match?.[1] ?? token();
          const stub = env.ROOMS.get(env.ROOMS.idFromName(id));
          const url = new URL(request.url);
          url.pathname = match ? `/${match[2] ?? ''}` : '/create';
          // The namespace ID stays server-side; the invitation is a random capability.
          const inner = new Request(url, request);
          inner.headers.set('X-Room-ID', id);
          response = await stub.fetch(inner);
        }
      } else response = json({ error: 'Not found' }, 404);
    } catch {
      // Never echo API/provider errors, request bodies or secrets to clients/logs.
      response = json({ error: 'The room service could not complete the request. Try again.' }, 503);
    }
    const headers = new Headers(response.headers);
    for (const [key, value] of Object.entries(cors)) headers.set(key, value);
    return new Response(response.body, { status: response.status, headers });
  },
};

export class NetplayRoom extends DurableObject {
  async fetch(request) {
    // Serialize the two-player reservation across awaits, including TURN issuance.
    return this.ctx.blockConcurrencyWhile(async () => {
      try { return await this.handle(request); }
      catch (error) {
        return json({ error: error instanceof RequestError ? error.message : 'Room request failed' },
          error instanceof RequestError ? 400 : 503);
      }
    });
  }

  async handle(request) {
    const path = new URL(request.url).pathname;
    const room = await this.ctx.storage.get('room');
    if (path === '/create' && request.method === 'POST') {
      if (room) return json({ error: 'Room already exists' }, 409);
      const body = await readJson(request);
      if (Object.keys(body).length) return json({ error: 'Invalid room request' }, 400);
      let ice;
      try { ice = await iceServers(this.env); }
      catch { return json({ error: 'Relay service is unavailable. Please try again later.' }, 503); }
      const owner = token();
      const expiresAt = Date.now() + ROOM_TTL;
      await this.ctx.storage.put('room', { owner, expiresAt, offer: null, guest: null, answer: null });
      await this.ctx.storage.setAlarm(expiresAt);
      return json({ id: request.headers.get('X-Room-ID'), owner, expiresAt, ...ice }, 201);
    }
    if (!room || room.expiresAt <= Date.now()) {
      if (room) await this.ctx.storage.deleteAll();
      return json({ error: 'This invitation has expired or ended. Ask the host for a new link.' }, 410);
    }
    const bearer = /^Bearer ([A-Za-z0-9_-]{22})$/.exec(request.headers.get('Authorization') ?? '')?.[1];
    if (path === '/join' && request.method === 'POST') {
      const body = await readJson(request);
      if (!TOKEN.test(body.guest ?? '')) return json({ error: 'Invalid join request' }, 400);
      if (!room.offer) return json({ error: 'The host is still preparing. Try again in a moment.' }, 409);
      if (room.guest && room.guest !== body.guest) return json({ error: 'This room already has two players.' }, 409);
      if (!room.guest) {
        let ice;
        try { ice = await iceServers(this.env); }
        catch { return json({ error: 'Relay service is unavailable. Please try again later.' }, 503); }
        room.guest = body.guest;
        room.guestIce = ice;
        await this.ctx.storage.put('room', room);
      }
      return json({ offer: room.offer, expiresAt: room.expiresAt, ...room.guestIce });
    }
    const owner = bearer === room.owner;
    const guest = room.guest && bearer === room.guest;
    if (!owner && !guest) return json({ error: 'Room access denied' }, 403);
    if (path === '/' && request.method === 'DELETE') {
      await this.ctx.storage.deleteAll();
      await this.ctx.storage.deleteAlarm();
      return json({ ended: true });
    }
    if (path === '/offer' && request.method === 'POST' && owner) {
      const body = await readJson(request);
      if (!connectionCode(body.code, 'offer')) return json({ error: 'Invalid offer' }, 400);
      if (room.offer && room.offer !== body.code) return json({ error: 'Start a new room to change the offer' }, 409);
      room.offer = body.code;
      await this.ctx.storage.put('room', room);
      return json({ ready: true });
    }
    if (path === '/answer' && request.method === 'POST' && guest) {
      const body = await readJson(request);
      if (!connectionCode(body.code, 'answer')) return json({ error: 'Invalid answer' }, 400);
      if (room.answer && room.answer !== body.code) return json({ error: 'Start a new room to change the answer' }, 409);
      room.answer = body.code;
      await this.ctx.storage.put('room', room);
      return json({ ready: true });
    }
    if (path === '/answer' && request.method === 'GET' && owner) return json({ answer: room.answer });
    return json({ error: 'Not found' }, 404);
  }

  async alarm() { await this.ctx.storage.deleteAll(); }
}
