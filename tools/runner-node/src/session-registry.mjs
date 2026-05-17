export class SessionAlreadyExistsError extends Error {
  constructor(sessionId) {
    super(`session already exists: ${sessionId}`);
    this.name = "SessionAlreadyExistsError";
    this.code = "WASMSH_SESSION_EXISTS";
    this.sessionId = sessionId;
  }
}

export function createSessionRegistry() {
  const sessions = new Map();
  return {
    add(session) {
      // Reject duplicates instead of silently overwriting an existing session.
      // Without this, a caller could resurrect or hijack another tenant's
      // session by replaying its ID against POST /sessions. Audit (D1).
      if (sessions.has(session.id)) {
        throw new SessionAlreadyExistsError(session.id);
      }
      sessions.set(session.id, session);
    },
    has(sessionId) {
      return sessions.has(sessionId);
    },
    get(sessionId) {
      return sessions.get(sessionId) ?? null;
    },
    delete(sessionId) {
      sessions.delete(sessionId);
    },
    list() {
      return Array.from(sessions.values()).map((session) => ({
        id: session.id,
        workerId: session.workerId,
      }));
    },
    values() {
      return Array.from(sessions.values());
    },
  };
}
