// SPDX-License-Identifier: GPL-3.0-or-later

// Canvas 2D presentation placement, shared by the copperline.dev/try page
// glue (try.js) and the single-file web player kit (kit-player.js). The
// functions take the canvas and its context as arguments and keep no state
// of their own beyond the staging canvas the caller holds.

// The element's backing-store size at device resolution, which a layout
// draw gives the canvas: the whole-number factors of integer scaling are
// real device pixels only then.
export function canvasDeviceSize(canvas, devicePixelRatio) {
  const dpr = devicePixelRatio || 1;
  return [
    Math.max(1, Math.round(canvas.clientWidth * dpr)),
    Math.max(1, Math.round(canvas.clientHeight * dpr)),
  ];
}

// WebEmu.present_layout's flat result as a record, or null while the core
// has no frame to place (it returns an empty array until the first one).
// `factors` is the integer scale pair, null for a smooth fit.
export function layoutFromArray(values) {
  if (!values || values.length < 10) return null;
  const [sx, sy, sw, sh, dx, dy, dw, dh, columns, lines] = values;
  return {
    sx,
    sy,
    sw,
    sh,
    dx,
    dy,
    dw,
    dh,
    factors: columns > 0 ? [columns, lines] : null,
  };
}

// Draw a layout with a 2D context. putImageData cannot scale, so the
// presentation goes through a staging canvas at its own size and is drawn
// from there into the layout's rect of a device-resolution backing store,
// point-sampled at whole-number factors and filtered for a smooth fit; the
// store outside the rect is black. `stage` is `{ canvas, ctx }`, created
// once by the caller and reused across frames.
export function drawPresentLayout(target, stage, view, width, rows, w, h, lay) {
  const { canvas, ctx } = target;
  if (stage.canvas.width !== width || stage.canvas.height !== rows) {
    stage.canvas.width = width;
    stage.canvas.height = rows;
  }
  stage.ctx.putImageData(new ImageData(view, width, rows), 0, 0);
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 0, w, h);
  ctx.imageSmoothingEnabled = !lay.factors;
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(stage.canvas, lay.sx, lay.sy, lay.sw, lay.sh, lay.dx, lay.dy, lay.dw, lay.dh);
}

// Emulator pixels per CSS pixel for pointer motion, one scale per axis.
// With a layout the picture's sub-rect occupies its destination rect of
// the backing store in device pixels; without one the picture fills
// `pictureScale` of the element on each axis (1 unless a bezel shrinks it).
// The two axis ratios differ because the bitmap is not the element's shape
// (a PAL capture is 668x540 shown as 4:3), and sharing one would skew
// vertical pointer speed.
export function cssToEmuScale(lay, presentSize, canvas, devicePixelRatio, pictureScale = 1) {
  if (lay) {
    const dpr = devicePixelRatio || 1;
    return { x: lay.sw / (lay.dw / dpr), y: lay.sh / (lay.dh / dpr) };
  }
  return {
    x: (presentSize.width || canvas.width) / (canvas.clientWidth * pictureScale),
    y: (presentSize.rows || canvas.height) / (canvas.clientHeight * pictureScale),
  };
}
