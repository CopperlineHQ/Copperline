// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { BinaryBitmap, HybridBinarizer, QRCodeReader, RGBLuminanceSource } from '@zxing/library';
import qrcode from '../../crates/copperline-web/www/netplay-qr.js';
import { inviteUrl } from '../../crates/copperline-web/www/netplay-room.js';
test('an independent QR decoder recovers the exact private invitation', () => {
  const invitation = inviteUrl('aB_0-9'.repeat(3) + 'xyZ1', 'https://copperline.dev/try/?rom=private');
  const qr = qrcode(0, 'M'); qr.addData(invitation); qr.make();
  const modules = qr.getModuleCount(), scale = 4, border = 4;
  const size = (modules + 2 * border) * scale;
  const pixels = new Uint8ClampedArray(size * size).fill(255);
  for (let y = 0; y < modules; y++) for (let x = 0; x < modules; x++) {
    if (!qr.isDark(y, x)) continue;
    for (let dy = 0; dy < scale; dy++) for (let dx = 0; dx < scale; dx++) {
      pixels[((y + border) * scale + dy) * size + (x + border) * scale + dx] = 0;
    }
  }
  const bitmap = new BinaryBitmap(new HybridBinarizer(new RGBLuminanceSource(pixels, size, size)));
  assert.equal(new QRCodeReader().decode(bitmap).getText(), invitation);
});
