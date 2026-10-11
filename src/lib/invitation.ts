/**
 * An invitation link's token, from the moment the page opens until it is
 * used or refused (task 051 Scope 12).
 *
 * The link is `<origin>/invite#<token>`. The token rides in the fragment,
 * which a browser never sends in a request line or a `Referer`, so it reaches
 * no access log. {@link takeInvitationFromLocation} moves it into
 * `sessionStorage` and drops the fragment from the address bar before any
 * request is made. A visitor who is not signed in signs in first, and the
 * sign-in callback always lands on `/`: the kept token is what brings them
 * back to the accept page there. It is removed the moment it is used or
 * refused.
 */

/** Where a token waits across a sign-in. Per tab, and gone with it. */
export const INVITATION_STORAGE_KEY = "rimaia.invitation";

/** The accept page's path: the app's one deep link. */
export const INVITATION_PATH = "/invite";

function storage(): Storage | null {
  try {
    return globalThis.sessionStorage ?? null;
  } catch {
    return null;
  }
}

/**
 * On `/invite#<token>`: keeps the token and replaces the address with
 * `/invite`, so neither a reload nor a copied address carries it. Anywhere
 * else it does nothing. Returns whether the page is the accept page.
 */
export function takeInvitationFromLocation(): boolean {
  if (window.location.pathname !== INVITATION_PATH) return false;
  const token = window.location.hash.replace(/^#/, "").trim();
  if (token !== "") {
    try {
      storage()?.setItem(INVITATION_STORAGE_KEY, token);
    } catch {
      // Without storage the token is lost on a sign-in; the page still works
      // for a visitor who is already signed in, from the address it had.
    }
    window.history.replaceState(window.history.state, "", INVITATION_PATH);
  }
  return true;
}

/** The token waiting to be used, or `null`. */
export function keptInvitation(): string | null {
  try {
    return storage()?.getItem(INVITATION_STORAGE_KEY) ?? null;
  } catch {
    return null;
  }
}

/** Forgets the token: it was used, refused, or set aside. */
export function forgetInvitation(): void {
  try {
    storage()?.removeItem(INVITATION_STORAGE_KEY);
  } catch {
    // Nothing kept, nothing to forget.
  }
}

/** Leaves the accept page for the app's root, without a reload. */
export function leaveInvitationPage(): void {
  if (window.location.pathname === INVITATION_PATH) {
    window.history.replaceState(window.history.state, "", "/");
  }
}
