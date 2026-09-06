// SPDX-License-Identifier: GPL-3.0-or-later
// Session media travels over the encrypted peer connection, never signaling.
export const MEDIA_VERSION = 'host-v1';
export const MEDIA_CHANNEL = 'copperline-setup-v1';
export const MEDIA_CHUNK = 16 * 1024;
const MANIFEST_LIMIT = 8192;
const MEDIA_LIMIT = 36 * 1024 * 1024;
const BUFFER_LIMIT = 256 * 1024;
const kinds = ['rom', 'ext', 'df0', 'df1'];
const digest = async bytes => [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))]
  .map(byte => byte.toString(16).padStart(2, '0')).join('');

export function validateManifest(value) {
  const config = value?.config;
  if (value?.type !== MEDIA_VERSION || !config ||
      !['A500', 'A1200'].includes(config.model) || !['PAL', 'NTSC'].includes(config.video) ||
      ![0, 100, 200, 400, 800].includes(config.floppySpeed) ||
      typeof config.floppySounds !== 'boolean' || typeof config.monoAudio !== 'boolean' ||
      typeof config.build !== 'string' || !config.build.length || config.build.length > 128 ||
      !Array.isArray(value.files) || !value.files.length || value.files.length > kinds.length) {
    throw new Error('Invalid host machine configuration');
  }
  const seen = new Set();
  let total = 0;
  const files = value.files.map(file => {
    if (!file || !kinds.includes(file.kind) || seen.has(file.kind) ||
        !Number.isSafeInteger(file.size) || file.size < 1 ||
        file.size > (['rom', 'ext'].includes(file.kind) ? 2 : 16) * 1024 * 1024 ||
        typeof file.hash !== 'string' || !/^[a-f0-9]{64}$/.test(file.hash) ||
        typeof file.label !== 'string' || !file.label.length || file.label.length > 256 ||
        typeof file.writable !== 'boolean') throw new Error('Invalid host media description');
    seen.add(file.kind);
    total += file.size;
    return { kind: file.kind, size: file.size, hash: file.hash, label: file.label, writable: file.writable };
  });
  if (!seen.has('rom') || total > MEDIA_LIMIT) throw new Error('Host media is too large or incomplete');
  return { type: MEDIA_VERSION, config: { model: config.model, video: config.video,
    floppySpeed: config.floppySpeed, floppySounds: config.floppySounds,
    monoAudio: config.monoAudio, build: config.build }, files };
}

export async function describeMedia(snapshot) {
  const media = [
    { kind: 'rom', bytes: snapshot.rom?.rom, label: snapshot.rom?.label, writable: false },
    { kind: 'ext', bytes: snapshot.rom?.ext, label: 'Extended ROM', writable: false },
    ...[0, 1].map(drive => ({ kind: `df${drive}`, bytes: snapshot.disks[drive]?.bytes,
      label: snapshot.disks[drive]?.name, writable: snapshot.disks[drive]?.writable })),
  ].filter(file => file.bytes != null);
  for (const file of media) {
    if (!(file.bytes instanceof Uint8Array)) throw new Error('Host media bytes are unavailable');
  }
  const config = { model: snapshot.model, video: snapshot.video, floppySpeed: snapshot.floppySpeed,
    floppySounds: snapshot.floppySounds, monoAudio: snapshot.monoAudio, build: snapshot.build };
  // Validate lengths before hashing or allocating any transport buffers.
  const manifest = validateManifest({ type: MEDIA_VERSION, config, files: media.map(file => ({
    kind: file.kind, size: file.bytes.length, hash: '0'.repeat(64),
    label: String(file.label ?? file.kind).slice(0, 256), writable: !!file.writable,
  })) });
  for (let i = 0; i < media.length; i++) manifest.files[i].hash = await digest(media[i].bytes);
  return { manifest, media };
}

export class MediaTransfer {
  constructor(channel, { host, progress = () => {}, fail = () => {} }) {
    this.channel = channel;
    this.host = host;
    this.progress = progress;
    this.fail = fail;
    this.closed = false;
    this.finished = false;
    this.abort = new AbortController();
    this.files = [];
    this.index = this.offset = this.received = 0;
    this.ready = new Promise((resolve, reject) => { this.opened = resolve; this.openFailed = reject; });
    this.done = new Promise((resolve, reject) => { this.complete = resolve; this.failed = reject; });
    // A remote close can arrive before the input channel invokes its onOpen.
    this.ready.catch(() => {});
    this.done.catch(() => {});
    channel.binaryType = 'arraybuffer';
    channel.bufferedAmountLowThreshold = BUFFER_LIMIT / 2;
    channel.onopen = () => { this.touch(); this.opened(); };
    channel.onmessage = event => {
      try { this.accept(event.data); } catch (error) { this.stop(error); }
    };
    channel.onclose = () => { if (!this.finished) this.stop(new Error('Game setup transfer was interrupted')); };
    channel.onerror = () => this.stop(new Error('Game setup transfer failed'));
    if (channel.readyState === 'open') channel.onopen();
  }

  touch() {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.stop(new Error('Game setup transfer timed out. Start a new session.')), 30000);
  }

  stop(error = new Error('Game setup transfer cancelled')) {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.timer);
    this.abort.abort(error);
    this.files = [];
    this.openFailed(error);
    this.failed(error);
    if (!this.finished) this.fail(error);
  }

  async write(data) {
    if (this.closed || this.channel.readyState !== 'open') throw new Error('Game setup transfer cancelled');
    if (this.channel.bufferedAmount > BUFFER_LIMIT) {
      await new Promise((resolve, reject) => {
        const cleanup = () => {
          this.channel.removeEventListener('bufferedamountlow', drained);
          this.abort.signal.removeEventListener('abort', cancelled);
        };
        const drained = () => { cleanup(); resolve(); };
        const cancelled = () => { cleanup(); reject(this.abort.signal.reason); };
        this.channel.addEventListener('bufferedamountlow', drained);
        this.abort.signal.addEventListener('abort', cancelled, { once: true });
        if (this.abort.signal.aborted) cancelled();
        else if (this.channel.bufferedAmount <= BUFFER_LIMIT / 2) drained();
      });
    }
    if (this.closed) throw new Error('Game setup transfer cancelled');
    this.channel.send(data);
    this.touch();
  }

  async send(snapshot) {
    if (!this.host || this.sending) throw new Error('Unexpected game setup transfer');
    this.sending = true;
    const { manifest, media } = await describeMedia(snapshot);
    await this.ready;
    const text = JSON.stringify(manifest);
    if (text.length > MANIFEST_LIMIT) throw new Error('Host media description is too large');
    await this.write(text);
    const total = manifest.files.reduce((sum, file) => sum + file.size, 0);
    let sent = 0;
    for (const file of media) {
      for (let offset = 0; offset < file.bytes.length; offset += MEDIA_CHUNK) {
        const chunk = file.bytes.subarray(offset, offset + MEDIA_CHUNK);
        await this.write(chunk);
        sent += chunk.length;
        this.progress('Sending', sent, total);
      }
    }
    this.sent = true;
    await this.done;
  }

  receive() {
    if (this.host) throw new Error('Unexpected game setup receiver');
    return this.done;
  }

  accept(data) {
    if (this.closed || this.finished) throw new Error('Unexpected game setup message');
    this.touch();
    if (this.host) {
      if (!this.sent || data !== '{"verified":true}') throw new Error('Invalid game setup acknowledgement');
      this.finished = true;
      clearTimeout(this.timer);
      this.complete();
      return;
    }
    if (!this.manifest) {
      if (typeof data !== 'string' || data.length > MANIFEST_LIMIT) throw new Error('Invalid host media description');
      this.manifest = validateManifest(JSON.parse(data));
      this.files = this.manifest.files.map(file => new Uint8Array(file.size));
      this.total = this.files.reduce((sum, file) => sum + file.length, 0);
      return;
    }
    const file = this.files[this.index];
    if (!(data instanceof ArrayBuffer) || !data.byteLength || data.byteLength > MEDIA_CHUNK ||
        !file || this.offset + data.byteLength > file.length) throw new Error('Invalid game setup chunk');
    file.set(new Uint8Array(data), this.offset);
    this.offset += data.byteLength;
    this.received += data.byteLength;
    this.progress('Receiving', this.received, this.total);
    if (this.offset === file.length) { this.index++; this.offset = 0; }
    if (this.index === this.files.length) this.verify().catch(error => this.stop(error));
  }

  async verify() {
    const files = this.files;
    for (let i = 0; i < files.length; i++) {
      if (await digest(files[i]) !== this.manifest.files[i].hash) throw new Error('Received game files did not match the host. Start a new session.');
      if (this.closed) return;
    }
    const snapshot = { ...this.manifest.config, rom: { rom: null, ext: null, label: '' }, disks: Array(4).fill(null) };
    this.manifest.files.forEach((file, i) => {
      if (file.kind === 'rom') { snapshot.rom.rom = files[i]; snapshot.rom.label = file.label; }
      else if (file.kind === 'ext') snapshot.rom.ext = files[i];
      else snapshot.disks[Number(file.kind.slice(2))] = { bytes: files[i], name: file.label, writable: file.writable };
    });
    await this.write('{"verified":true}');
    if (this.closed) return;
    this.finished = true;
    this.files = [];
    clearTimeout(this.timer);
    this.complete(snapshot);
  }
}
