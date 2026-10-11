import { useEffect, useState } from "react";

import { acceptInvitation, previewInvitation, toRimaiaError } from "../lib/commands";
import { forgetInvitation } from "../lib/invitation";
import type { InvitationPreview, RimaiaError, Role, TeamListing } from "../types";

/** What each role lets a person do, in one line (ADR-0029 point 3). */
export const ROLE_MEANINGS: Readonly<Record<Role, string>> = {
  owner: "Owner: can invite and remove people, and change the team's repositories and settings",
  member: "Member: can create, edit and run the team's tasks",
};

type State =
  | { readonly kind: "reading" }
  | { readonly kind: "preview"; readonly preview: InvitationPreview; readonly joining: boolean }
  | { readonly kind: "refused"; readonly error: RimaiaError };

interface InviteViewProps {
  /** The invitation token the app kept from the link's fragment. */
  readonly token: string;
  /** Called with the team once the person has joined it. */
  readonly onJoined: (team: TeamListing) => void;
  /** Leaves the page without joining. */
  readonly onClose: () => void;
}

/**
 * The accept page (task 051 Scope 12, ADR-0029 point 4): what the link
 * admits its holder to, before they join, and **Join team**.
 *
 * The token travels in a command's body, never in a URL. It is forgotten the
 * moment it is used or refused, and a link that cannot be used shows the
 * server's one sentence for every such link and nothing about any team.
 */
export function InviteView({ token, onJoined, onClose }: InviteViewProps) {
  const [state, setState] = useState<State>({ kind: "reading" });
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    document.title = "Invitation · Rimaia";
    let active = true;
    previewInvitation(token).then(
      (preview) => {
        if (active) setState({ kind: "preview", preview, joining: false });
      },
      (thrown) => {
        forgetInvitation();
        if (active) setState({ kind: "refused", error: toRimaiaError(thrown) });
      },
    );
    return () => {
      active = false;
    };
  }, [token]);

  async function join(preview: InvitationPreview) {
    setState({ kind: "preview", preview, joining: true });
    setError(null);
    try {
      const team = await acceptInvitation(token);
      forgetInvitation();
      onJoined(team);
    } catch (thrown) {
      forgetInvitation();
      setState({ kind: "preview", preview, joining: false });
      setError(toRimaiaError(thrown));
    }
  }

  function decline() {
    forgetInvitation();
    onClose();
  }

  return (
    <main className="sign-in invite-view">
      <div className="sign-in-card panel invite-card">
        <span className="sidebar-mark" aria-hidden="true">
          R
        </span>
        {state.kind === "reading" && <p className="muted">Reading the invitation…</p>}
        {state.kind === "refused" && (
          <>
            <h1>Invitation</h1>
            <p className="invite-refusal" role="alert">
              {state.error.message}
            </p>
            <button type="button" onClick={onClose}>
              Go to your board
            </button>
          </>
        )}
        {state.kind === "preview" && (
          <>
            <h1>Join {state.preview.teamName}</h1>
            <dl className="detail-list invite-details">
              <dt>Team</dt>
              <dd>{state.preview.teamName}</dd>
              <dt>Role</dt>
              <dd>{ROLE_MEANINGS[state.preview.role]}</dd>
              <dt>Invited by</dt>
              <dd>{state.preview.invitedByLogin ?? "a former member"}</dd>
              <dt>Link expires</dt>
              <dd className="tabular-nums">{formatExpiry(state.preview.expiresAt)}</dd>
            </dl>
            {error && (
              <p className="invite-refusal" role="alert">
                {error.message}
              </p>
            )}
            <div className="account-dialog-actions">
              {error === null && (
                <button
                  type="button"
                  className="btn-primary"
                  disabled={state.joining}
                  onClick={() => join(state.preview)}
                >
                  {state.joining ? "Joining…" : "Join team"}
                </button>
              )}
              <button type="button" onClick={decline} disabled={state.joining}>
                {error === null ? "Not now" : "Go to your board"}
              </button>
            </div>
          </>
        )}
      </div>
    </main>
  );
}

function formatExpiry(timestamp: string): string {
  const at = new Date(timestamp);
  if (Number.isNaN(at.getTime())) return timestamp;
  return at.toLocaleString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
