// SPDX-License-Identifier: GPL-3.0-or-later

// DOM-free adaptive render-stride controller, split from try.js so its
// overload and recovery hysteresis can be regression-tested under Node.

export const DEFAULT_RENDER_STRIDE_CONFIG = Object.freeze({
  enterMs: 16,
  exitMs: 14.5,
  enterHoldMs: 500,
  exitHoldMs: 2000,
  sampleWeight: 0.2,
});

export function newRenderStrideState() {
  return {
    avgStepMs: 0,
    avgFrameStepMs: 0,
    active: false,
    transitionSinceMs: null,
  };
}

export function resetRenderStrideState(state) {
  state.avgStepMs = 0;
  state.avgFrameStepMs = 0;
  state.active = false;
  state.transitionSinceMs = null;
}

export function cancelRenderStrideTransition(state) {
  state.transitionSinceMs = null;
}

export function updateRenderStrideState(
  state,
  nowMs,
  stepElapsed,
  stepped,
  rendered,
  config = DEFAULT_RENDER_STRIDE_CONFIG,
) {
  // Raw whole-call cost remains the signal for the starved-rAF fallback,
  // including deferred work: it asks whether another call now would
  // monopolise the main thread.
  state.avgStepMs = state.avgStepMs * 0.9 + stepElapsed * 0.1;

  // Compare like with like. run_hidden omits the framebuffer render, so
  // feeding its cheaper cost into the full-render EWMA made render stride
  // lower its own recovery signal and oscillate under a steady workload.
  if (!rendered) return;

  // `run` advances fixed 1/60-second pacing slices, not necessarily one
  // hardware video field. Normalize catch-up calls by the work completed;
  // an idle rendering call pulls the average down because it proves the
  // host caught up.
  const frameStepMs = stepped > 0 ? stepElapsed / stepped : 0;
  if (state.avgFrameStepMs === 0 && frameStepMs > 0) {
    state.avgFrameStepMs = frameStepMs;
  } else {
    state.avgFrameStepMs =
      state.avgFrameStepMs * (1 - config.sampleWeight) +
      frameStepMs * config.sampleWeight;
  }

  const threshold = state.active ? config.exitMs : config.enterMs;
  const nextActive = state.avgFrameStepMs > threshold;
  if (nextActive === state.active) {
    state.transitionSinceMs = null;
    return;
  }
  if (state.transitionSinceMs === null) {
    state.transitionSinceMs = nowMs;
    return;
  }
  const holdMs = nextActive ? config.enterHoldMs : config.exitHoldMs;
  if (nowMs - state.transitionSinceMs >= holdMs) {
    state.active = nextActive;
    state.transitionSinceMs = null;
  }
}
