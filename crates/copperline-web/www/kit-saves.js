// SPDX-License-Identifier: GPL-3.0-or-later

// Per-title persistence for the single-file web player kit: the browser
// twin of the native player's per-user game directory. A page packed by
// tools/publish-web keeps the visitor's writable copy of each disk (and the
// quick save state, when the manifest enables it) in an IndexedDB database
// named after the manifest's `[game] id`, so two titles served from one
// origin never see each other's saves.
//
// A stored record carries the SHA-256 of the pristine image it was written
// over. A page repacked with a changed disk no longer matches and starts
// afresh, as the native player replaces its per-user copy of an updated
// ADF. Storage can be missing or refused (private windows, some file://
// origins); every operation then resolves to "nothing stored" rather than
// throwing, and the page simply plays without persistence.

const STORE = 'kv';

export function saveDbName(gameId) {
  return `copperline-kit-${gameId}`;
}

export function diskKey(index) {
  return `disk:${index}`;
}

export const QUICK_STATE_KEY = 'state:quick';

// Whether a stored record belongs to this build's pristine image.
export function recordMatches(record, sha256) {
  return (
    !!record &&
    typeof record === 'object' &&
    record.sha256 === sha256 &&
    record.bytes instanceof Uint8Array &&
    record.bytes.length > 0
  );
}

export function makeRecord(sha256, bytes, nowMs) {
  return { sha256, bytes, savedAt: nowMs };
}

export function bytesEqual(a, b) {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

// A download name for an exported disk or state: the title squeezed to a
// portable file-name stem, then the suffix.
export function exportFileName(title, suffix) {
  const stem = String(title)
    .replace(/[^A-Za-z0-9._-]+/g, '-')
    .replace(/^[-.]+|[-.]+$/g, '');
  return `${stem || 'copperline'}${suffix}`;
}

function requestDone(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

// The store behind one title. `idb` is the IndexedDB factory (the page
// passes globalThis.indexedDB; absent, nothing persists).
export function openSaveStore(gameId, idb) {
  let dbPromise = null;
  const open = () => {
    if (!idb) return Promise.reject(new Error('no IndexedDB'));
    if (!dbPromise) {
      dbPromise = new Promise((resolve, reject) => {
        let request;
        try {
          request = idb.open(saveDbName(gameId), 1);
        } catch (e) {
          reject(e);
          return;
        }
        request.onupgradeneeded = () => request.result.createObjectStore(STORE);
        request.onsuccess = () => {
          const db = request.result;
          // A connection the browser takes away (site data cleared, another
          // tab upgrading the database) must not stay cached: the next
          // operation opens a fresh one.
          const forget = () => {
            dbPromise = null;
          };
          db.onclose = forget;
          db.onversionchange = () => {
            db.close();
            forget();
          };
          resolve(db);
        };
        request.onerror = () => reject(request.error);
        request.onblocked = () => reject(new Error('save database is blocked'));
      });
      // A failed open is retried on the next call instead of being cached.
      dbPromise.catch(() => {
        dbPromise = null;
      });
    }
    return dbPromise;
  };
  const run = async (mode, op) => {
    const db = await open();
    const tx = db.transaction(STORE, mode);
    // Armed before the request is issued, so a transaction that commits
    // the moment its one request succeeds cannot be missed.
    const committed = new Promise((resolve, reject) => {
      tx.oncomplete = resolve;
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
    // A failed request rejects below; its transaction's own rejection must
    // not then surface as an unhandled one.
    committed.catch(() => {});
    const result = await requestDone(op(tx.objectStore(STORE)));
    if (mode === 'readwrite') await committed;
    return result;
  };
  return {
    // Resolves to the record, or null when nothing (usable) is stored.
    async get(key) {
      try {
        return (await run('readonly', (store) => store.get(key))) ?? null;
      } catch {
        return null;
      }
    },
    // Resolves to whether the record was stored.
    async put(key, record) {
      try {
        await run('readwrite', (store) => store.put(record, key));
        return true;
      } catch {
        return false;
      }
    },
    async remove(key) {
      try {
        await run('readwrite', (store) => store.delete(key));
        return true;
      } catch {
        return false;
      }
    },
  };
}
