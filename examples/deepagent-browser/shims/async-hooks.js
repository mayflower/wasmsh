// Minimal AsyncLocalStorage shim for browsers.
// LangGraph uses AsyncLocalStorage for context propagation.
export class AsyncLocalStorage {
  _store = undefined;
  getStore() { return this._store; }
  run(store, callback) {
    const prev = this._store;
    this._store = store;
    try { return callback(); }
    finally { this._store = prev; }
  }
  enterWith(store) { this._store = store; }
  disable() { this._store = undefined; }
}
