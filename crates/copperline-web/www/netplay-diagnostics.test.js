// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { NetplayDiagnostics, connectionFailure } from './netplay-diagnostics.js';
test('diagnostics retain route and DTLS evidence after teardown without network addresses or credentials', async () => {
  const secret = 'never-export-this';
  const stats = new Map([
    ['t', { type: 'transport', selectedCandidatePairId: 'p', dtlsState: 'failed', tlsVersion: secret }],
    ['p', { type: 'candidate-pair', state: 'succeeded', nominated: true, localCandidateId: 'l', remoteCandidateId: 'r', currentRoundTripTime: 0.02, bytesSent: 12 }],
    ['l', { type: 'local-candidate', candidateType: 'relay', protocol: 'udp', address: secret, url: secret, usernameFragment: secret }],
    ['r', { type: 'remote-candidate', candidateType: 'host', address: secret }],
  ]);
  const pc = { connectionState: 'failed', iceConnectionState: 'connected', iceGatheringState: 'complete', signalingState: 'stable',
    localDescription: { sdp: `a=candidate:${secret} 1 udp 1 ${secret} 10 typ relay\r\na=ice-pwd:${secret}` },
    getStats: async () => stats };
  const diagnostics = new NetplayDiagnostics();
  diagnostics.record('peer-state', pc);
  diagnostics.iceError(701);
  diagnostics.iceError(secret);
  await diagnostics.capture(pc);
  pc.connectionState = 'closed';
  const report = await diagnostics.report(pc, null);
  assert.equal(report.connection.peer, 'failed');
  assert.equal(report.connection.localCandidates.relay, 1);
  assert.equal(report.stats.dtls, 'failed');
  assert.equal(report.stats.selectedPair.local, 'relay');
  assert.deepEqual(report.iceErrorCodes, [701]);
  assert.ok(!JSON.stringify(report).includes(secret));
  assert.match(connectionFailure(pc), /network route opened/);
  pc.connectionState = 'failed'; pc.iceConnectionState = 'failed';
  assert.match(connectionFailure(pc), /discovery failed/);
  pc.getStats = () => { throw new Error(secret); };
  await diagnostics.capture(pc);
  assert.equal(diagnostics.stats.dtls, 'failed');
});
