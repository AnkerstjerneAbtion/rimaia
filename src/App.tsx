import { useCallback, useEffect, useRef, useState } from "react";

import { DoctorBanner } from "./components/DoctorBanner";
import { ErrorBanner } from "./components/ErrorBanner";
import { Sidebar } from "./components/Sidebar";
import { useDoctor } from "./hooks/useDoctor";
import { useTasks } from "./hooks/useTasks";
import { subscribeToSignedOut, subscribeToStaleBundle } from "./lib/auth";
import { useClientCapabilities } from "./lib/capabilities";
import { getAccount, getAppInfo, getReviewDigest, listTeams, toRimaiaError } from "./lib/commands";
import { subscribeToEventStreamFailure } from "./lib/events";
import { openingView, reviewQueue } from "./lib/review";
import { chooseTeam, fallbackTeam, startingTeam } from "./lib/team";
import { TeamsProvider } from "./lib/teams";
import { AccountView } from "./views/AccountView";
import { BoardView } from "./views/BoardView";
import { ReviewView } from "./views/ReviewView";
import { AnalyticsView } from "./views/AnalyticsView";
import { RunsView } from "./views/RunsView";
import { SettingsView } from "./views/SettingsView";
import { SignInView } from "./views/SignInView";
import { WelcomeView } from "./views/WelcomeView";
import type { RimaiaError, TeamListing, View } from "./types";
import "./styles.css";

/** What a page that is older than its server, and could not fix that by
 *  reloading, says (task 050 Scope 6). */
export const STALE_BUNDLE_MESSAGE =
  "This page is older than the server, and reloading did not fix it. Try again in a minute.";

function App() {
  // Solo is the desktop alone: no sign-in, no account page, no switcher
  // (ADR-0030 point 7, ADR-0029 point 2). Everything else is a client of a
  // server, which first has to know who is asking and which team it is about.
  const { mode } = useClientCapabilities();
  return mode === "solo" ? <Shell /> : <SignedInApp />;
}

type Phase =
  | { readonly kind: "loading" }
  | { readonly kind: "signed-out" }
  | { readonly kind: "failed"; readonly error: RimaiaError }
  | { readonly kind: "ready"; readonly teams: readonly TeamListing[]; readonly currentId: string };

/**
 * A client of a server: signed in, with a team chosen, or neither.
 *
 * The only requests before a team is chosen are `get_account` and `list_teams`
 * (task 050 Scope 6), because a person in two teams has an entity-less board
 * call refused until one is named, and a read sent first would be that
 * refusal. `unauthenticated` from anywhere returns to the sign-in screen
 * through `src/lib/auth.ts`: no component below handles it.
 */
function SignedInApp() {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [stale, setStale] = useState(false);
  // The team on screen, readable from the stream's callback, which outlives
  // the render it was made in.
  const onScreen = useRef<string | null>(null);
  onScreen.current = phase.kind === "ready" ? phase.currentId : null;

  useEffect(() => {
    let active = true;
    (async () => {
      try {
        await getAccount();
        const teams = await listTeams();
        if (!active) return;
        const start = startingTeam(teams);
        if (!start) {
          setPhase({
            kind: "failed",
            error: { code: "invalid", message: "You are not in any team yet." },
          });
          return;
        }
        chooseTeam(start.id);
        setPhase({ kind: "ready", teams, currentId: start.id });
      } catch (thrown) {
        if (!active) return;
        const error = toRimaiaError(thrown);
        setPhase(
          error.code === "unauthenticated" ? { kind: "signed-out" } : { kind: "failed", error },
        );
      }
    })();
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    const stopSignedOut = subscribeToSignedOut(() => setPhase({ kind: "signed-out" }));
    const stopStale = subscribeToStaleBundle(() => setStale(true));
    return () => {
      stopSignedOut();
      stopStale();
    };
  }, []);

  // A stream refused `not_found` is a stream for a team the person no longer
  // belongs to. Ask again which teams they have: if the one on screen is gone,
  // fall back to the personal team, or to the first when there is none.
  useEffect(
    () =>
      subscribeToEventStreamFailure(({ error }) => {
        if (error.code !== "not_found") return;
        listTeams().then(
          (teams) => {
            if (onScreen.current === null) return;
            if (teams.some((team) => team.id === onScreen.current)) return;
            const fallback = fallbackTeam(teams);
            if (!fallback) {
              setPhase({ kind: "failed", error });
              return;
            }
            chooseTeam(fallback.id);
            setPhase({ kind: "ready", teams, currentId: fallback.id });
          },
          () => {},
        );
      }),
    [],
  );

  const choose = useCallback((teamId: string) => {
    chooseTeam(teamId);
    setPhase((current) =>
      current.kind === "ready" ? { ...current, currentId: teamId } : current,
    );
  }, []);

  if (phase.kind === "loading") return <div className="app" />;
  if (phase.kind === "signed-out") return <SignInView />;
  if (phase.kind === "failed") {
    return (
      <main className="sign-in">
        <ErrorBanner error={phase.error} />
      </main>
    );
  }

  const current = phase.teams.find((team) => team.id === phase.currentId) as TeamListing;
  return (
    <TeamsProvider value={{ teams: phase.teams, current, choose }}>
      {/* Keyed by team, so no open panel keeps an id from the team it left. */}
      <Shell key={current.id} staleBundle={stale} />
    </TeamsProvider>
  );
}

function Shell({ staleBundle = false }: { readonly staleBundle?: boolean }) {
  // Route state, not a router. Five views with no URLs, no nesting and no deep
  // links to preserve — a router library would be all cost (task 001).
  //
  // `null` is "we do not yet know where to start": the opening view depends on
  // `onboardingDismissed`, and defaulting to the board would flash it before
  // the welcome screen replaced it on a first run (seam-contract D22).
  const [view, setView] = useState<View | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  // `get_app_info` and `run_doctor` read this machine, and onboarding checks
  // it: a client with none (a browser, task 050) calls neither, never opens on
  // the welcome screen, and shows no app version.
  const { mode } = useClientCapabilities();
  const hasMachine = mode !== "browser";
  const { report, dismiss } = useDoctor(hasMachine);
  // The sidebar's count of what is waiting for a verdict, off the board's own
  // read so it agrees with the queue the review view walks.
  const { state: board } = useTasks(null);
  const reviewCount = reviewQueue(board.tasks).length;

  useEffect(() => {
    if (!hasMachine) {
      // Onboarding has been done by whoever runs the runners. A failed digest
      // read opens the board, as a failed `get_app_info` does below.
      getReviewDigest().then(
        (digest) => setView(openingView(true, digest)),
        () => setView("board"),
      );
      return;
    }
    getAppInfo().then(
      async (info) => {
        setAppVersion(info.appVersion);
        // A digest is only worth reading once the welcome screen is out of the
        // way; a failed read opens the board, for the same reason a failed
        // `get_app_info` does below.
        const digest = info.onboardingDismissed
          ? await getReviewDigest().catch(() => null)
          : null;
        setView(openingView(info.onboardingDismissed, digest));
      },
      // A failed read is not a reason to withhold the app. The board is the
      // safe answer: the welcome screen is skippable, and showing it to a
      // returning user would be worse than not showing it to a new one.
      () => setView("board"),
    );
  }, [hasMachine]);

  if (view === null) return <div className="app" />;

  return (
    <div className="app">
      <Sidebar
        current={view}
        onNavigate={setView}
        version={appVersion}
        reviewCount={reviewCount}
      />
      <main className="content">
        {staleBundle && (
          <ErrorBanner error={{ code: "upgrade_required", message: STALE_BUNDLE_MESSAGE }} />
        )}
        {/* Above every view, not only Settings: a queue that will not start
            tonight is worth interrupting the board for now. Suppressed on the
            welcome screen, which reports the same checks per step. A browser
            has no machine to check. */}
        {hasMachine && view !== "welcome" && (
          <DoctorBanner
            report={report}
            onOpenSettings={() => setView("settings")}
            onDismiss={(result) => void dismiss(result)}
          />
        )}
        {view === "board" && <BoardView />}
        {view === "review" && <ReviewView />}
        {view === "runs" && <RunsView />}
        {view === "analytics" && <AnalyticsView />}
        {view === "settings" && <SettingsView />}
        {view === "account" && <AccountView />}
        {view === "welcome" && <WelcomeView onFinish={() => setView("board")} />}
      </main>
    </div>
  );
}

export default App;
