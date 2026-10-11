import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  createInvitation,
  deleteTeam,
  leaveTeam,
  listInvitations,
  listTeamMembers,
  removeMember,
  revokeInvitation,
  setMemberRole,
  toRimaiaError,
} from "../../lib/commands";
import { subscribeToTeamsChanged } from "../../lib/events";
import { INVITATION_PATH } from "../../lib/invitation";
import { useTeams } from "../../lib/teams";
import type { Invitation, RimaiaError, Role, TeamListing, TeamMember } from "../../types";
import { useAccountRead } from "../account/useAccountRead";

/** What the server answers a personal team's own person (task 051 Scope 4),
 *  shown before they try. */
export const PERSONAL_TEAM_KEEPS_ITS_PERSON =
  "a personal team always keeps the person it belongs to as an owner";

/** The last-owner rule's sentence, as the server words it. */
export function needsAnOwner(teamName: string): string {
  return `${teamName} needs at least one owner: make someone else an owner first`;
}

/** Why the person cannot leave the team on screen, or `null` when they can. */
export function whyNotLeave(team: TeamListing): string | null {
  if (team.personal) return PERSONAL_TEAM_KEEPS_ITS_PERSON;
  if (team.role === "owner" && team.ownerCount <= 1) return needsAnOwner(team.name);
  return null;
}

const ROLE_LABELS: Readonly<Record<Role, string>> = { owner: "Owner", member: "Member" };

/**
 * Settings → Team (task 051 Scope 12, ADR-0029 points 3, 4 and 6): who is in
 * the team on screen, and for its owners the controls that change that.
 *
 * Rendered wherever the team switcher is, and never in solo, which has one
 * team and no people to manage. Every rule here is the server's; the section
 * only disables what the server would refuse, with the server's sentence.
 */
export function TeamSection() {
  const teams = useTeams();
  if (!teams) return null;
  return <TeamPanel team={teams.current} reload={teams.reload} />;
}

function TeamPanel({
  team,
  reload,
}: {
  readonly team: TeamListing;
  readonly reload: (select?: string) => Promise<void>;
}) {
  const isOwner = team.role === "owner";
  const members = useAccountRead(() => listTeamMembers(team.id));
  const reloadMembers = members.reload;
  const [error, setError] = useState<RimaiaError | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | null = null;
    subscribeToTeamsChanged(() => reloadMembers()).then(
      (stop) => {
        if (active) unlisten = stop;
        else stop();
      },
      () => {},
    );
    return () => {
      active = false;
      unlisten?.();
    };
  }, [reloadMembers]);

  async function act(write: () => Promise<void>, after: () => Promise<void> | void) {
    setBusy(true);
    setError(null);
    try {
      await write();
      await after();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setBusy(false);
    }
  }

  const leaveRefusal = whyNotLeave(team);

  return (
    <section className="panel team-section" aria-labelledby="team-heading">
      <h3 id="team-heading">{team.name}</h3>
      <p className="muted">
        {team.personal
          ? "Your personal team. You can invite people into it; it is deleted with your account."
          : `You are ${team.role === "owner" ? "an owner" : "a member"} of this team.`}
      </p>
      {(members.error ?? error) && (
        <ErrorBanner
          error={(members.error ?? error) as RimaiaError}
          onDismiss={error ? () => setError(null) : undefined}
        />
      )}

      <h4 className="team-subheading">Members</h4>
      {members.value === null && !members.error && <p className="muted">Reading…</p>}
      {members.value && (
        <ul className="account-rows team-members">
          {members.value.map((member) => (
            <MemberRow
              key={member.userId}
              member={member}
              canManage={isOwner}
              busy={busy}
              onRole={(role) =>
                act(
                  () => setMemberRole(team.id, member.userId, role),
                  () => reload().then(reloadMembers),
                )
              }
              onRemove={() =>
                act(
                  () => removeMember(team.id, member.userId),
                  () => reload().then(reloadMembers),
                )
              }
            />
          ))}
        </ul>
      )}

      <div className="team-leave">
        <button
          type="button"
          disabled={busy || leaveRefusal !== null}
          aria-describedby={leaveRefusal ? "team-leave-why" : undefined}
          onClick={() => act(() => leaveTeam(team.id), () => reload())}
        >
          Leave team
        </button>
        {leaveRefusal && (
          <p className="muted" id="team-leave-why">
            {leaveRefusal}
          </p>
        )}
      </div>

      {isOwner && <InvitationsArea teamId={team.id} />}
      {isOwner && !team.personal && (
        <DeleteTeamArea team={team} onDeleted={() => reload()} />
      )}
    </section>
  );
}

function MemberRow({
  member,
  canManage,
  busy,
  onRole,
  onRemove,
}: {
  readonly member: TeamMember;
  readonly canManage: boolean;
  readonly busy: boolean;
  readonly onRole: (role: Role) => void;
  readonly onRemove: () => void;
}) {
  const fixed = member.isPersonalOwner;
  return (
    <li className="account-row team-member">
      <div className="account-row-main">
        <span className="account-row-title">
          {member.login}
          {!canManage && (
            <span className="account-row-flag">{ROLE_LABELS[member.role].toLowerCase()}</span>
          )}
          {member.isPersonalOwner && <span className="account-row-flag">this team's person</span>}
        </span>
      </div>
      {canManage && (
        <div className="team-member-controls">
          <label className="team-role">
            <select
              aria-label={`Role of ${member.login}`}
              value={member.role}
              disabled={busy || fixed}
              onChange={(event) => onRole(event.target.value as Role)}
            >
              <option value="owner">{ROLE_LABELS.owner}</option>
              <option value="member">{ROLE_LABELS.member}</option>
            </select>
          </label>
          <button
            type="button"
            disabled={busy || fixed}
            onClick={onRemove}
            aria-label={`Remove ${member.login}`}
          >
            Remove
          </button>
        </div>
      )}
    </li>
  );
}

/**
 * An owner's invitations: a role and **Create link**, the link once, and the
 * pending ones with **Revoke**. The rest are the team's record of who let whom
 * in, folded away.
 */
function InvitationsArea({ teamId }: { readonly teamId: string }) {
  const invitations = useAccountRead(() => listInvitations(teamId));
  const reloadInvitations = invitations.reload;
  const [role, setRole] = useState<Role>("member");
  const [link, setLink] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | null = null;
    subscribeToTeamsChanged(() => reloadInvitations()).then(
      (stop) => {
        if (active) unlisten = stop;
        else stop();
      },
      () => {},
    );
    return () => {
      active = false;
      unlisten?.();
    };
  }, [reloadInvitations]);

  async function create(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    setCopied(false);
    try {
      const created = await createInvitation(teamId, role);
      // The token lives in this component's state only, and only until the
      // owner is done with it: never stored, never re-read.
      setLink(`${window.location.origin}${INVITATION_PATH}#${created.token}`);
      reloadInvitations();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setBusy(false);
    }
  }

  async function copy() {
    if (link === null) return;
    try {
      await navigator.clipboard.writeText(link);
      setCopied(true);
    } catch {
      setError({ code: "internal", message: "could not copy the link to the clipboard" });
    }
  }

  async function revoke(invitation: Invitation) {
    setBusy(true);
    setError(null);
    try {
      await revokeInvitation(invitation.id);
      reloadInvitations();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setBusy(false);
    }
  }

  const pending = (invitations.value ?? []).filter((invitation) => invitation.status === "pending");
  const history = (invitations.value ?? []).filter((invitation) => invitation.status !== "pending");

  return (
    <div className="team-invitations">
      <h4 className="team-subheading">Invite people</h4>
      <p className="muted">
        A link works once, for anyone signed in, with the role you choose here.
      </p>
      {(invitations.error ?? error) && (
        <ErrorBanner
          error={(invitations.error ?? error) as RimaiaError}
          onDismiss={error ? () => setError(null) : undefined}
        />
      )}
      <form className="team-invite-form" onSubmit={create}>
        <label className="account-field team-invite-role">
          Role
          <select value={role} onChange={(event) => setRole(event.target.value as Role)}>
            <option value="member">{ROLE_LABELS.member}</option>
            <option value="owner">{ROLE_LABELS.owner}</option>
          </select>
        </label>
        <button type="submit" disabled={busy}>
          Create link
        </button>
      </form>
      {link !== null && (
        <div className="account-dialog team-link" role="group" aria-label="New invitation link">
          <p>
            <strong>This link is shown once and works for seven days.</strong>
          </p>
          <code className="account-secret">{link}</code>
          <div className="account-dialog-actions">
            <button type="button" onClick={copy}>
              {copied ? "Copied" : "Copy link"}
            </button>
            <button type="button" className="btn-primary" onClick={() => setLink(null)}>
              Done
            </button>
          </div>
        </div>
      )}
      {pending.length > 0 && (
        <ul className="account-rows team-pending">
          {pending.map((invitation) => (
            <li key={invitation.id} className="account-row">
              <div className="account-row-main">
                <span className="account-row-title">
                  {`${ROLE_LABELS[invitation.role]} link`}
                  <span className="account-row-flag">pending</span>
                </span>
                <span className="muted tabular-nums">
                  {`Made by ${invitation.createdByLogin ?? "a former member"} · expires ${day(
                    invitation.expiresAt,
                  )}`}
                </span>
              </div>
              <button
                type="button"
                disabled={busy}
                onClick={() => revoke(invitation)}
                aria-label={`Revoke the ${invitation.role} link made ${day(invitation.createdAt)}`}
              >
                Revoke
              </button>
            </li>
          ))}
        </ul>
      )}
      {history.length > 0 && (
        <details className="team-history">
          <summary>{`Past invitations (${history.length})`}</summary>
          <ul className="account-rows">
            {history.map((invitation) => (
              <li key={invitation.id} className="account-row">
                <div className="account-row-main">
                  <span className="account-row-title">
                    {`${ROLE_LABELS[invitation.role]} link`}
                    <span className="account-row-flag">{invitation.status}</span>
                  </span>
                  <span className="muted">{historyLine(invitation)}</span>
                </div>
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

/** Who made a past invitation, and what became of it. */
function historyLine(invitation: Invitation): string {
  const by = invitation.createdByLogin ?? "a former member";
  switch (invitation.status) {
    case "accepted":
      return `Made by ${by}, used by ${invitation.acceptedByLogin ?? "a former member"} ${day(
        invitation.acceptedAt,
      )}`;
    case "revoked":
      return `Made by ${by}, revoked ${day(invitation.revokedAt)}`;
    default:
      return `Made by ${by}, expired ${day(invitation.expiresAt)}`;
  }
}

/**
 * Deleting the team: what goes and what stays, then the team's name typed
 * exactly. The server checks the name too, so every door is held to it.
 */
function DeleteTeamArea({
  team,
  onDeleted,
}: {
  readonly team: TeamListing;
  readonly onDeleted: () => Promise<void>;
}) {
  const [typed, setTyped] = useState("");
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const matches = typed === team.name;

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!matches || deleting) return;
    setDeleting(true);
    setError(null);
    try {
      await deleteTeam(team.id, typed);
      await onDeleted();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
      setDeleting(false);
    }
  }

  return (
    <form className="account-confirm team-delete" onSubmit={submit} aria-label="Delete team">
      <h4 className="team-subheading">Delete team</h4>
      <p>
        {`Deleting ${team.name} deletes every task, run, transcript and repository record in it, for everyone in it. `}
        Each runner keeps the worktrees on its own machine; its owner removes those.
      </p>
      <label className="account-field">
        {`Type ${team.name} to confirm`}
        <input
          type="text"
          value={typed}
          onChange={(event) => setTyped(event.target.value)}
          autoComplete="off"
        />
      </label>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <div className="account-dialog-actions">
        <button type="submit" className="btn-danger" disabled={!matches || deleting}>
          {deleting ? "Deleting…" : "Delete team"}
        </button>
      </div>
    </form>
  );
}

function day(timestamp: string | null): string {
  if (timestamp === null) return "";
  const at = new Date(timestamp);
  if (Number.isNaN(at.getTime())) return "";
  return at.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
