// Source of truth for the copperline.dev/try page glue: published to the
// website repository by .github/workflows/wasm-demo.yml alongside the wasm
// bundle, so this JS and the WebEmu API always change together.
// Copperline audio worklet: receives interleaved stereo f32 chunks (44.1 kHz)
// from the main thread via postMessage and plays them back in 128-frame
// quanta. Single-threaded pipeline: no SharedArrayBuffer, just transferred
// buffers. Underruns emit silence; a queue past MAX_QUEUE_FRAMES drops the
// oldest chunks (timeline jump or a backgrounded tab). Queue depth is
// reported back every ~10 quanta so the pacer can trim drift.

const MAX_QUEUE_FRAMES = 11025; // ~250 ms at 44.1 kHz
const PREBUFFER_FRAMES = 2646; // ~60 ms: gate playback until this much queued

class CopperlineAudio extends AudioWorkletProcessor {
  constructor() {
    super();
    this.chunks = [];
    this.offset = 0; // read offset (in floats) into chunks[0]
    this.queuedFrames = 0;
    this.underruns = 0;
    this.quanta = 0;
    this.prebuffering = true;
    this.port.onmessage = (e) => {
      const chunk = e.data;
      if (!(chunk instanceof Float32Array) || chunk.length < 2) return;
      this.chunks.push(chunk);
      this.queuedFrames += chunk.length >> 1;
      while (this.queuedFrames > MAX_QUEUE_FRAMES && this.chunks.length > 1) {
        const dropped = this.chunks.shift();
        this.queuedFrames -= (dropped.length >> 1) - (this.offset >> 1);
        this.offset = 0;
      }
    };
  }

  process(inputs, outputs) {
    const left = outputs[0][0];
    const right = outputs[0].length > 1 ? outputs[0][1] : outputs[0][0];
    // After a start or a hard drain, hold silence until a small cushion is
    // queued so playback does not stutter through the refill.
    if (this.prebuffering) {
      if (this.queuedFrames < PREBUFFER_FRAMES) return true;
      this.prebuffering = false;
    }
    let i = 0;
    while (i < left.length && this.chunks.length > 0) {
      const chunk = this.chunks[0];
      while (i < left.length && this.offset < chunk.length) {
        left[i] = chunk[this.offset];
        right[i] = chunk[this.offset + 1];
        this.offset += 2;
        this.queuedFrames--;
        i++;
      }
      if (this.offset >= chunk.length) {
        this.chunks.shift();
        this.offset = 0;
      }
    }
    if (i < left.length) {
      this.underruns++;
      this.prebuffering = true;
      for (; i < left.length; i++) {
        left[i] = 0;
        right[i] = 0;
      }
    }
    if (++this.quanta % 10 === 0) {
      this.port.postMessage({
        queuedMs: (this.queuedFrames / sampleRate) * 1000,
        underruns: this.underruns,
      });
    }
    return true;
  }
}

registerProcessor('copperline-audio', CopperlineAudio);
