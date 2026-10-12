/**
 * Two facts about the session that the transports learn and exactly one place
 * in the interface reacts to (task 050 Scope 6): the credential stopped working,
 * and the bundle on screen is older than the server.
 *
 * Nothing here imports another module of the app, so the transports in
 * `commands.ts` and `client.ts` can raise either without a cycle, and no
 * component handles `unauthenticated` itself: `App` subscribes once and puts
 * the sign-in screen up, whichever command or stream noticed.
 */

type Listener = () => void;

const signedOutListeners = new Set<Listener>();
const staleBundleListeners = new Set<Listener>();

function subscribe(listeners: Set<Listener>, listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function raise(listeners: Set<Listener>) {
  for (const listener of [...listeners]) listener();
}

/** Hears every `unauthenticated` from a board command or an event stream. */
export function subscribeToSignedOut(listener: Listener): () => void {
  return subscribe(signedOutListeners, listener);
}

/** The session ended: a command or a stream was refused `unauthenticated`, or
 *  the person signed out. */
export function reportSignedOut(): void {
  raise(signedOutListeners);
}

/** Hears that the server is newer than this page and a reload did not fix it. */
export function subscribeToStaleBundle(listener: Listener): () => void {
  return subscribe(staleBundleListeners, listener);
}

/** This bundle was already reloaded into and is still too old. */
export function reportStaleBundle(): void {
  raise(staleBundleListeners);
}
