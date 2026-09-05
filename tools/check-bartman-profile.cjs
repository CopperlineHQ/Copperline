// SPDX-License-Identifier: GPL-3.0-or-later
// Compile BartmanAbyss/vscode-amiga-debug with npm ci and npx tsc -p . first.
// Usage: node tools/check-bartman-profile.cjs /path/to/vscode-amiga-debug FILE...
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const [upstream, ...files] = process.argv.slice(2);
if (!upstream || !files.length) {
  console.error('usage: check-bartman-profile.cjs UPSTREAM_CHECKOUT FILE...');
  process.exit(2);
}
const { ProfileFile } = require(path.resolve(upstream, 'out/backend/profile.js'));
for (const file of files) {
  // Upstream uses buffer.buffer without byteOffset. Give it a dedicated buffer.
  const bytes = fs.readFileSync(file);
  const input = Buffer.allocUnsafeSlow(bytes.length);
  bytes.copy(input);
  const profile = new ProfileFile(input);
  assert.ok(profile.frames.length >= 1 && profile.frames.length <= 100);
  for (const frame of profile.frames) {
    assert.equal(frame.dmaRecords.length, 227 * 313);
    assert.equal(frame.customRegs.length, 256);
    assert.equal(frame.screenshotType, 'png');
    assert.deepEqual(Buffer.from(frame.screenshot.slice(0, 8)),
      Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    assert.ok(frame.profileArray.length > 0);
    let samples = 0;
    for (let i = 0; i < frame.profileArray.length; ++i) {
      if (frame.profileArray[i] >= 0xffff0000) {
        assert.ok(i + 17 < frame.profileArray.length, 'complete register sample');
        i += 17;
        ++samples;
      }
    }
    assert.ok(samples > 0);
  }
  console.log(`${file}: ${profile.frames.length} frames, ${profile.sectionBases.length} hunks; upstream parser OK`);
}
