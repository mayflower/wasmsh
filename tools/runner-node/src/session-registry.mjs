export function createSessionRegistry() {
  const sessions = new Map();
  return {
    add(session) {
      sessions.set(session.id, session);
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
