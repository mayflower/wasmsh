/**
 * Shared test session helper — manages session lifecycle and cleanup.
 */
import { after } from "node:test";

export function createSessionTracker(createNodeSession, assetDir) {
  const sessions = [];

  after(async () => {
    for (const s of sessions) {
      try { await s.close(); } catch { /* ignore */ }
    }
  });

  return async function openSession(options = {}) {
    const session = await createNodeSession({
      assetDir,
      ...options,
    });
    sessions.push(session);
    return session;
  };
}
