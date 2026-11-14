// SPDX-License-Identifier: GPL-3.0-or-later

import assert from 'node:assert/strict';
import test from 'node:test';
import {
  QUICK_STATE_KEY,
  bytesEqual,
  diskKey,
  exportFileName,
  makeRecord,
  openSaveStore,
  recordMatches,
  saveDbName,
} from './kit-saves.js';

// The slice of IndexedDB the store uses, with requests settling on later
// microtasks the way the real thing settles them on later tasks.
function fakeIndexedDb({ failOpen = false, failPut = false } = {}) {
  const databases = new Map();
  const settle = (request, fn) => {
    queueMicrotask(() => {
      try {
        request.result = fn();
        request.onsuccess?.();
      } catch (e) {
        request.error = e;
        request.onerror?.();
      }
    });
    return request;
  };
  return {
    databases,
    opens: 0,
    open(name) {
      this.opens++;
      const request = {};
      queueMicrotask(() => {
        if (failOpen) {
          request.error = new Error('storage is disabled');
          request.onerror?.();
          return;
        }
        let data = databases.get(name);
        const db = {
          closed: false,
          close() {
            this.closed = true;
          },
          createObjectStore: () => {
            data = new Map();
            databases.set(name, data);
          },
          transaction: (storeName, mode) => {
            if (db.closed) throw new Error('InvalidStateError: the connection is closed');
            const tx = {};
            let failed = false;
            const done = () =>
              queueMicrotask(() => queueMicrotask(() => (failed ? tx.onabort?.() : tx.oncomplete?.())));
            tx.objectStore = () => ({
              get: (key) => (done(), settle({}, () => data.get(key))),
              put: (value, key) => {
                if (failPut) {
                  failed = true;
                  tx.error = new Error('quota exceeded');
                  done();
                  return settle({}, () => {
                    throw tx.error;
                  });
                }
                done();
                return settle({}, () => void data.set(key, value));
              },
              delete: (key) => (done(), settle({}, () => void data.delete(key))),
            });
            assert.ok(mode === 'readonly' || mode === 'readwrite');
            return tx;
          },
        };
        request.result = db;
        this.last = db;
        if (!data) request.onupgradeneeded?.();
        request.onsuccess?.();
      });
      return request;
    },
  };
}

test('keys and names keep titles apart', () => {
  assert.equal(saveDbName('skyhawks'), 'copperline-kit-skyhawks');
  assert.notEqual(saveDbName('a'), saveDbName('b'));
  assert.equal(diskKey(0), 'disk:0');
  assert.notEqual(diskKey(1), QUICK_STATE_KEY);
});

test('a record only matches the image it was written over', () => {
  const record = makeRecord('abc', new Uint8Array([1, 2, 3]), 1234);
  assert.deepEqual(record, { sha256: 'abc', bytes: new Uint8Array([1, 2, 3]), savedAt: 1234 });
  assert.equal(recordMatches(record, 'abc'), true);
  assert.equal(recordMatches(record, 'def'), false);
  assert.equal(recordMatches(null, 'abc'), false);
  assert.equal(recordMatches({ sha256: 'abc', bytes: new Uint8Array(0) }, 'abc'), false);
  assert.equal(recordMatches({ sha256: 'abc', bytes: [1, 2, 3] }, 'abc'), false);
  assert.equal(recordMatches('abc', 'abc'), false);
});

test('byte comparison', () => {
  const a = new Uint8Array([1, 2, 3]);
  assert.equal(bytesEqual(a, a), true);
  assert.equal(bytesEqual(a, new Uint8Array([1, 2, 3])), true);
  assert.equal(bytesEqual(a, new Uint8Array([1, 2, 4])), false);
  assert.equal(bytesEqual(a, new Uint8Array([1, 2])), false);
  assert.equal(bytesEqual(a, null), false);
});

test('export names are portable file names', () => {
  assert.equal(exportFileName('Sky Hawks: Reloaded!', '.png'), 'Sky-Hawks-Reloaded.png');
  assert.equal(exportFileName('Game-save.adf', ''), 'Game-save.adf');
  assert.equal(exportFileName('../../etc', '.adf'), 'etc.adf');
  assert.equal(exportFileName('???', '.png'), 'copperline.png');
});

test('records round-trip through the store, one database per title', async () => {
  const idb = fakeIndexedDb();
  const store = openSaveStore('game-a', idb);
  assert.equal(await store.get(diskKey(0)), null);
  const record = makeRecord('abc', new Uint8Array([9, 8, 7]), 1);
  assert.equal(await store.put(diskKey(0), record), true);
  assert.deepEqual(await store.get(diskKey(0)), record);
  assert.equal(await store.get(diskKey(1)), null);
  assert.equal(idb.opens, 1, 'the database is opened once and reused');

  const other = openSaveStore('game-b', idb);
  assert.equal(await other.get(diskKey(0)), null);
  assert.deepEqual([...idb.databases.keys()], ['copperline-kit-game-a', 'copperline-kit-game-b']);

  assert.equal(await store.remove(diskKey(0)), true);
  assert.equal(await store.get(diskKey(0)), null);
});

test('missing or refusing storage reads as nothing stored', async () => {
  const none = openSaveStore('game', undefined);
  assert.equal(await none.get(diskKey(0)), null);
  assert.equal(await none.put(diskKey(0), makeRecord('a', new Uint8Array([1]), 0)), false);
  assert.equal(await none.remove(diskKey(0)), false);

  const refused = fakeIndexedDb({ failOpen: true });
  const store = openSaveStore('game', refused);
  assert.equal(await store.get(diskKey(0)), null);
  assert.equal(await store.put(diskKey(0), makeRecord('a', new Uint8Array([1]), 0)), false);
  assert.equal(refused.opens, 2, 'a failed open is retried, not cached');

  const throwing = openSaveStore('game', {
    open() {
      throw new Error('SecurityError');
    },
  });
  assert.equal(await throwing.get(diskKey(0)), null);

  const full = openSaveStore('game', fakeIndexedDb({ failPut: true }));
  assert.equal(await full.put(diskKey(0), makeRecord('a', new Uint8Array([1]), 0)), false);
});

test('a connection the browser takes away is reopened, not reused', async () => {
  const idb = fakeIndexedDb();
  const store = openSaveStore('game', idb);
  const record = makeRecord('abc', new Uint8Array([4, 5]), 2);
  assert.equal(await store.put(diskKey(0), record), true);
  assert.equal(idb.opens, 1);

  // Another tab asks to upgrade: this one lets go of its connection.
  const first = idb.last;
  first.onversionchange();
  assert.equal(first.closed, true);
  assert.deepEqual(await store.get(diskKey(0)), record);
  assert.equal(idb.opens, 2);

  // The browser closes it outright (site data cleared under the page).
  const second = idb.last;
  second.closed = true;
  second.onclose();
  assert.equal(await store.put(diskKey(1), record), true);
  assert.equal(idb.opens, 3);
});
