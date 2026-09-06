// SPDX-License-Identifier: GPL-3.0-or-later
// Host-controlled media commits on a confirmed, stopped rollback boundary.
export const SWAP_VERSION = 'disk-v1';
export const SWAP_CHANNEL = 'copperline-disks-v1';
export const DISK_LIMIT = 16 * 1024 * 1024;
const CHUNK = 16 * 1024;
const BUFFER = 256 * 1024;
const hex = bytes => [...bytes].map(byte => byte.toString(16).padStart(2, '0')).join('');
const digest = async bytes => hex(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)));
const frameNumber = value => Number.isSafeInteger(value) && value >= 0;

export function validateDisk(value) {
  if (!value || ![0, 1].includes(value.drive) || !Number.isSafeInteger(value.size) ||
      value.size < 0 || value.size > DISK_LIMIT || typeof value.writable !== 'boolean' ||
      typeof value.name !== 'string' || value.name.length > 256 ||
      typeof value.hash !== 'string' || !/^[a-f0-9]{64}$/.test(value.hash) ||
      (!value.size && value.writable)) throw new Error('Invalid replacement disk description');
  return { drive: value.drive, size: value.size, writable: value.writable, name: value.name, hash: value.hash };
}

export class DiskSwaps {
  constructor(channel, { host, machine, status = () => {}, changed = () => {}, fail = () => {} }) {
    Object.assign(this, { channel, host, machine, status, changed, fail });
    this.abort = new AbortController();
    this.id = 0;
    this.busy = this.closed = false;
    channel.binaryType = 'arraybuffer';
    channel.bufferedAmountLowThreshold = BUFFER / 2;
    channel.onmessage = event => {
      try { this.accept(event.data); } catch (error) { this.stop(error); }
    };
    channel.onclose = () => this.stop(new Error('Disk swap connection ended'));
    channel.onerror = () => this.stop(new Error('Disk swap connection failed'));
  }

  emu() {
    if (this.closed) throw new Error('Disk swap cancelled');
    const machine = this.machine();
    if (!machine?.netplay_status()[0]) throw new Error('Wait for the game to connect before changing disks');
    return machine;
  }

  touch() {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.stop(new Error('Disk swap timed out')), 30000);
  }

  begin() {
    this.busy = true;
    this.touch();
    this.deadline = setTimeout(() => this.stop(new Error('Disk swap exceeded three minutes')), 180000);
    this.status('Pausing both players for a disk change...');
    this.changed();
  }

  finish() {
    clearTimeout(this.timer);
    clearTimeout(this.deadline);
    const disk = this.disk;
    this.disk = this.bytes = this.pending = null;
    this.busy = false;
    this.phase = null;
    this.status(disk.size ? `DF${disk.drive}: ${disk.name} — both players resumed` : `DF${disk.drive} ejected — both players resumed`);
    this.changed(disk);
  }

  stop(error = new Error('Disk swap cancelled')) {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.timer);
    clearTimeout(this.deadline);
    this.abort.abort(error);
    this.pending?.reject(error);
    this.disk = this.bytes = this.pending = null;
    this.busy = false;
    this.fail(error);
  }

  async write(data) {
    if (this.closed || this.channel.readyState !== 'open') throw new Error('Disk swap connection is unavailable');
    if (this.channel.bufferedAmount > BUFFER) {
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
        else if (this.channel.bufferedAmount <= BUFFER / 2) drained();
      });
    }
    if (this.closed) throw new Error('Disk swap cancelled');
    this.channel.send(data);
    this.touch();
  }

  send(type, fields = {}) { return this.write(JSON.stringify({ type, id: this.id, ...fields })); }

  expect(type) {
    if (this.pending) throw new Error('Overlapping disk swap reply');
    const result = new Promise((resolve, reject) => { this.pending = { type, resolve, reject }; });
    result.catch(() => {});
    return result;
  }

  async exchange(type, reply, fields) {
    const response = this.expect(reply);
    await this.send(type, fields);
    return response;
  }

  async ready() {
    while (!this.emu().netplay_swap_ready()) {
      await new Promise((resolve, reject) => {
        const cancelled = () => { clearTimeout(timer); reject(this.abort.signal.reason); };
        const timer = setTimeout(() => { this.abort.signal.removeEventListener('abort', cancelled); resolve(); }, 20);
        this.abort.signal.addEventListener('abort', cancelled, { once: true });
        if (this.abort.signal.aborted) cancelled();
      });
    }
    return hex(this.emu().netplay_swap_digest());
  }

  async swap(drive, disk) {
    if (!this.host || this.busy || this.closed) throw new Error('Only the host can start one disk change at a time');
    if (this.channel.readyState !== 'open') throw new Error('The disk transfer channel is not ready yet');
    const bytes = disk?.bytes ?? new Uint8Array();
    if (!(bytes instanceof Uint8Array) || bytes.length > DISK_LIMIT || (disk && !bytes.length)) throw new Error('Select a disk image of up to 16 MiB');
    // Invalid local files leave the current game running.
    this.emu().netplay_validate_disk(drive, bytes, !!disk?.writable);
    this.begin();
    try {
      const description = validateDisk({ drive, size: bytes.length, writable: !!disk?.writable,
        name: String(disk?.name ?? '').slice(0, 256), hash: await digest(bytes) });
      this.disk = description;
      this.id++;
      const frame = this.emu().netplay_hold();
      const held = await this.exchange('begin', 'held', { disk: description });
      if (!frameNumber(held.frame) || Math.abs(held.frame - frame) > 32) throw new Error('Invalid peer disk swap frame');
      const target = Math.max(frame, held.frame);
      this.emu().netplay_stop_at(target);
      const [peer, own] = await Promise.all([this.exchange('target', 'ready', { frame: target }), this.ready()]);
      if (peer.hash !== own) throw new Error('Players differ before the disk change');
      this.emu().netplay_stage_disk(drive, bytes, description.writable);
      const prepared = this.expect('prepared');
      for (let offset = 0; offset < bytes.length; offset += CHUNK) {
        await this.write(bytes.subarray(offset, offset + CHUNK));
        this.status(`Sending replacement disk: ${Math.floor(Math.min(offset + CHUNK, bytes.length) * 100 / bytes.length)}%`);
      }
      await this.send('end');
      await prepared;
      this.emu().netplay_apply_disk();
      const applied = await this.exchange('apply', 'applied');
      if (applied.hash !== hex(this.emu().netplay_swap_digest())) throw new Error('Players differ after the disk change');
      await this.send('resume');
      this.emu().netplay_resume();
      this.finish();
    } catch (error) { this.stop(error); throw error; }
  }

  accept(data) {
    if (this.closed) return;
    this.touch();
    if (data instanceof ArrayBuffer) {
      if (this.host || this.phase !== 'receiving' || !data.byteLength || data.byteLength > CHUNK ||
          this.offset + data.byteLength > this.bytes.length) throw new Error('Invalid replacement disk chunk');
      this.bytes.set(new Uint8Array(data), this.offset);
      this.offset += data.byteLength;
      this.status(`Receiving replacement disk: ${Math.floor(this.offset * 100 / this.bytes.length)}%`);
      return;
    }
    if (typeof data !== 'string' || data.length > 2048) throw new Error('Invalid disk swap message');
    const message = JSON.parse(data);
    if (!message || !Number.isSafeInteger(message.id)) throw new Error('Invalid disk swap identifier');
    if (this.host) {
      if (message.id !== this.id || message.type !== this.pending?.type) throw new Error('Unexpected disk swap reply');
      const { resolve } = this.pending;
      this.pending = null;
      resolve(message);
      return;
    }
    if (message.type === 'begin') {
      if (this.busy || message.id !== this.id + 1) throw new Error('Unexpected disk swap request');
      const disk = validateDisk(message.disk);
      const frame = this.emu().netplay_hold();
      this.id = message.id;
      this.disk = disk;
      this.phase = 'held';
      this.begin();
      this.send('held', { frame }).catch(error => this.stop(error));
      return;
    }
    if (!this.busy || message.id !== this.id) throw new Error('Unexpected disk swap identifier');
    if (message.type === 'target' && this.phase === 'held') {
      if (!frameNumber(message.frame)) throw new Error('Invalid disk swap frame');
      this.emu().netplay_stop_at(message.frame);
      this.phase = 'waiting';
      this.ready().then(hash => {
        if (this.closed) return;
        this.bytes = new Uint8Array(this.disk.size);
        this.offset = 0;
        this.phase = 'receiving';
        return this.send('ready', { hash });
      }).catch(error => this.stop(error));
    } else if (message.type === 'end' && this.phase === 'receiving' && this.offset === this.disk.size) {
      this.phase = 'verifying';
      this.verify().catch(error => this.stop(error));
    } else if (message.type === 'apply' && this.phase === 'prepared') {
      this.emu().netplay_apply_disk();
      this.phase = 'applied';
      this.send('applied', { hash: hex(this.emu().netplay_swap_digest()) }).catch(error => this.stop(error));
    } else if (message.type === 'resume' && this.phase === 'applied') {
      this.emu().netplay_resume();
      this.finish();
    } else throw new Error('Unexpected disk swap phase');
  }

  async verify() {
    if (await digest(this.bytes) !== this.disk.hash) throw new Error('Replacement disk did not match the host');
    if (this.closed) return;
    this.emu().netplay_stage_disk(this.disk.drive, this.bytes, this.disk.writable);
    this.bytes = null;
    this.phase = 'prepared';
    await this.send('prepared');
  }
}
