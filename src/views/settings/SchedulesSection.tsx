import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  createSchedule,
  deleteSchedule,
  listSchedules,
  listTimezones,
  previewSchedulePreflight,
  setScheduleEnabled,
  toRimaiaError,
  updateSchedule,
} from "../../lib/commands";
import { subscribeToSchedulesChanged } from "../../lib/events";
import type {
  PreflightSummary,
  RimaiaError,
  ScheduleInput,
  ScheduleMode,
  ScheduleView,
} from "../../types";

/**
 * Settings → Schedules (task 013, ADR-0010).
 *
 * The panel exists for one moment in particular: 18:00, the user is leaving,
 * and the only thing that can tell them the nightly queue is set up correctly
 * is **the next fire time on the row**. Task 013's Scope says so in as many
 * words — "so a wrong cron expression is caught in the evening rather than
 * discovered in the morning" — so that column is the one thing here that is not
 * optional, and it is rendered absolute *and* relative because the two catch
 * different mistakes. "22:00" looks right when the zone is wrong; "in 14 hours"
 * does not.
 *
 * A row whose expression cannot be read at all shows `nextFireError` in place
 * of a time rather than a blank, and the list still renders — this is where a
 * broken schedule is fixed, so it has to be visible here above all.
 *
 * # No date picker, and no timezone package
 *
 * `<input type="datetime-local">`, `<input type="time">` and a `<select>` fed
 * by {@link listTimezones}. The zone list comes from the backend's own
 * `chrono-tz` table, which is what makes a pickable name a storable name by
 * construction and keeps seam-contract D6's dependency list where it is.
 *
 * The one-off time is entered in the *browser's* local zone and stored as an
 * absolute instant, which is unambiguous and deliberately unrelated to the
 * schedule's IANA zone. That zone governs the repeating expression and the stop
 * time, both of which are wall-clock, and neither of which an instant can
 * express.
 *
 * # The form is a form, not commit-on-blur
 *
 * Same argument `McpSection` makes for its port field and `ConcurrencySection`
 * for its limit: a cron expression is half-invalid for most of the time it is
 * being typed, and "0 2" on the way to "0 22 * * *" must not be saved. The
 * enable toggle is the exception and commits immediately, because it has no
 * invalid value and is the control a user reaches for in a hurry.
 */
export function SchedulesSection() {
  const [schedules, setSchedules] = useState<ScheduleView[] | null>(null);
  const [timezones, setTimezones] = useState<string[]>([]);
  const [readError, setReadError] = useState<RimaiaError | null>(null);
  const [saveError, setSaveError] = useState<RimaiaError | null>(null);
  const [saving, setSaving] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);

  const [previewing, setPreviewing] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreflightSummary | null>(null);
  const [previewError, setPreviewError] = useState<RimaiaError | null>(null);

  const read = useCallback(async () => {
    try {
      setSchedules(await listSchedules());
      setReadError(null);
    } catch (thrown) {
      setReadError(toRimaiaError(thrown));
    }
  }, []);

  useEffect(() => {
    void read();
  }, [read]);

  useEffect(() => {
    // The zone table never changes while the app is open, so it is read once
    // rather than on every schedules change. A failure here is not surfaced as
    // an error banner: the list is only ever used to fill a `<select>`, and an
    // empty one already shows as an empty picker.
    listTimezones().then(setTimezones, () => setTimezones([]));
  }, []);

  useEffect(() => {
    const subscription = subscribeToSchedulesChanged(() => {
      void read();
    });
    return () => {
      void subscription.then((unlisten) => unlisten());
    };
  }, [read]);

  function startAdding() {
    setEditing("new");
    setDraft(emptyDraft(timezones));
    setSaveError(null);
  }

  function startEditing(schedule: ScheduleView) {
    setEditing(schedule.id);
    setDraft(draftOf(schedule));
    setSaveError(null);
  }

  function stopEditing() {
    setEditing(null);
    setDraft(null);
    setSaveError(null);
  }

  async function handleSave(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (draft === null || editing === null || saving) {
      return;
    }

    setSaving(true);
    setSaveError(null);
    try {
      const input = toInput(draft);
      if (editing === "new") {
        await createSchedule(input);
      } else {
        await updateSchedule(editing, input);
      }
      // The list re-reads itself on `schedules:changed`; this only closes the
      // form, and only once the write has actually landed.
      stopEditing();
    } catch (thrown) {
      setSaveError(toRimaiaError(thrown));
    } finally {
      setSaving(false);
    }
  }

  async function handleToggle(schedule: ScheduleView) {
    setSaveError(null);
    try {
      await setScheduleEnabled(schedule.id, !schedule.enabled);
    } catch (thrown) {
      setSaveError(toRimaiaError(thrown));
    }
  }

  async function handleDelete(schedule: ScheduleView) {
    setSaveError(null);
    try {
      await deleteSchedule(schedule.id);
    } catch (thrown) {
      setSaveError(toRimaiaError(thrown));
    }
  }

  async function handlePreview(schedule: ScheduleView) {
    setPreviewing(schedule.id);
    setPreview(null);
    setPreviewError(null);
    try {
      setPreview(await previewSchedulePreflight(schedule.id));
    } catch (thrown) {
      setPreviewError(toRimaiaError(thrown));
    } finally {
      setPreviewing(null);
    }
  }

  return (
    <section className="panel schedules-section">
      <h3>Schedules</h3>
      <p className="muted">
        Start the queue at a chosen time, with an optional stop time. Reaching the stop time
        starts nothing new and lets the run in flight finish.
      </p>

      {readError && <ErrorBanner error={readError} onDismiss={() => setReadError(null)} />}
      {schedules === null && !readError && <p className="muted">Reading…</p>}

      {schedules !== null && schedules.length === 0 && editing === null && (
        <p className="muted">
          No schedules yet — the queue starts only when you press Start on the Runs view.
        </p>
      )}

      {schedules !== null && schedules.length > 0 && (
        <ul className="schedule-list">
          {schedules.map((schedule) => (
            <li key={schedule.id} className="schedule-row">
              <div className="schedule-row-heading">
                <strong>{schedule.name}</strong>
                <label className="schedule-enabled">
                  <input
                    type="checkbox"
                    aria-label={`Enable ${schedule.name}`}
                    checked={schedule.enabled}
                    onChange={() => void handleToggle(schedule)}
                  />
                  {schedule.enabled ? "On" : "Off"}
                </label>
              </div>

              {/* One caption, not two paragraphs: when it repeats, when it
                  stops and how much it runs at once are one fact about a
                  schedule, and reading them as two rows is what made twenty
                  schedules unscannable. Each half keeps its own element so it
                  is still addressable on its own. */}
              <p className="schedule-captions">
                <span className="schedule-kind">{describe(schedule)}</span>
                <span className="schedule-configuration">
                  {schedule.stopAt ? `Stops at ${schedule.stopAt}` : "No stop time"} ·{" "}
                  {schedule.mode === "parallel"
                    ? `${schedule.maxConcurrency} at once`
                    : "One at a time"}
                </span>
              </p>

              {/* The reason this panel exists. Absolute and relative together:
                  "22:00" looks right when the zone is wrong, and "in 14 hours"
                  does not. */}
              <p className={schedule.nextFireError ? "schedule-broken" : "schedule-next-fire"}>
                {schedule.nextFireError
                  ? `Will not fire: ${schedule.nextFireError}`
                  : schedule.nextFireAt
                    ? `Next: ${absolute(schedule.nextFireAt)} (${relative(schedule.nextFireAt)})`
                    : "Next: never again — this one-off time has already run"}
              </p>

              <div className="schedule-row-actions">
                <button
                  type="button"
                  onClick={() => void handlePreview(schedule)}
                  disabled={previewing !== null}
                >
                  {previewing === schedule.id ? "Checking…" : "Preview"}
                </button>
                <button type="button" onClick={() => startEditing(schedule)}>
                  Edit
                </button>
                <button type="button" onClick={() => void handleDelete(schedule)}>
                  Delete
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {preview && (
        <p role="status">
          {preview.scheduleName} will start {preview.plan.filter((e) => e.skip === null).length} of{" "}
          {preview.plan.length} ready {preview.plan.length === 1 ? "task" : "tasks"}
          {preview.closesAt ? `, until ${absolute(preview.closesAt)}` : ""}.
          {preview.plan.some((entry) => entry.skip !== null) &&
            ` Passing over: ${preview.plan
              .filter((entry) => entry.skip !== null)
              .map((entry) => `${entry.title} (${explain(entry.skip)})`)
              .join(", ")}.`}
        </p>
      )}
      {previewError && (
        <ErrorBanner error={previewError} onDismiss={() => setPreviewError(null)} />
      )}

      {editing === null ? (
        <button type="button" onClick={startAdding}>
          Add a schedule
        </button>
      ) : (
        draft !== null && (
          <form className="schedule-form" onSubmit={handleSave}>
            <label htmlFor="schedule-name">Name</label>
            <input
              id="schedule-name"
              value={draft.name}
              disabled={saving}
              onChange={(event) => setDraft({ ...draft, name: event.target.value })}
            />

            <div role="radiogroup" aria-label="When it runs">
              <label>
                <input
                  type="radio"
                  name="schedule-kind"
                  aria-label="Repeating"
                  checked={draft.kind === "recurring"}
                  disabled={saving}
                  onChange={() => setDraft({ ...draft, kind: "recurring" })}
                />
                Repeating
              </label>
              <label>
                <input
                  type="radio"
                  name="schedule-kind"
                  aria-label="Once"
                  checked={draft.kind === "once"}
                  disabled={saving}
                  onChange={() => setDraft({ ...draft, kind: "once" })}
                />
                Once
              </label>
            </div>

            {draft.kind === "recurring" ? (
              <>
                <label htmlFor="schedule-cron">Repeats</label>
                <input
                  id="schedule-cron"
                  value={draft.cron}
                  placeholder="0 22 * * *"
                  disabled={saving}
                  onChange={(event) => setDraft({ ...draft, cron: event.target.value })}
                />
              </>
            ) : (
              <>
                <label htmlFor="schedule-start-at">Starts at</label>
                <input
                  id="schedule-start-at"
                  type="datetime-local"
                  value={draft.startAt}
                  disabled={saving}
                  onChange={(event) => setDraft({ ...draft, startAt: event.target.value })}
                />
              </>
            )}

            <label htmlFor="schedule-timezone">Timezone</label>
            <select
              id="schedule-timezone"
              value={draft.timezone}
              disabled={saving}
              onChange={(event) => setDraft({ ...draft, timezone: event.target.value })}
            >
              {timezones.map((zone) => (
                <option key={zone} value={zone}>
                  {zone}
                </option>
              ))}
            </select>

            <label htmlFor="schedule-stop-at">Stops at</label>
            <input
              id="schedule-stop-at"
              type="time"
              value={draft.stopAt}
              disabled={saving}
              onChange={(event) => setDraft({ ...draft, stopAt: event.target.value })}
            />

            <label htmlFor="schedule-mode">Runs</label>
            <select
              id="schedule-mode"
              value={draft.mode}
              disabled={saving}
              onChange={(event) =>
                setDraft({ ...draft, mode: event.target.value as ScheduleMode })
              }
            >
              <option value="sequential">One at a time</option>
              <option value="parallel">Several at once</option>
            </select>

            {draft.mode === "parallel" && (
              <>
                <label htmlFor="schedule-max-concurrency">At once</label>
                <input
                  id="schedule-max-concurrency"
                  type="number"
                  min={1}
                  value={draft.maxConcurrency}
                  disabled={saving}
                  onChange={(event) =>
                    setDraft({ ...draft, maxConcurrency: event.target.value })
                  }
                />
              </>
            )}

            <div className="schedule-form-actions">
              <button type="submit" disabled={saving}>
                {editing === "new" ? "Create" : "Save"}
              </button>
              <button type="button" onClick={stopEditing} disabled={saving}>
                Cancel
              </button>
              {saving && <span className="muted">Saving…</span>}
            </div>
          </form>
        )
      )}

      {saveError && <ErrorBanner error={saveError} onDismiss={() => setSaveError(null)} />}
    </section>
  );
}

/** The form's own state. Strings throughout, because that is what an `<input>`
 *  holds — the conversion to a {@link ScheduleInput} happens once, on submit,
 *  in {@link toInput}. */
interface Draft {
  name: string;
  kind: "recurring" | "once";
  cron: string;
  /** `YYYY-MM-DDTHH:mm`, as `<input type="datetime-local">` produces it. */
  startAt: string;
  timezone: string;
  /** `HH:mm`, as `<input type="time">` produces it. */
  stopAt: string;
  mode: ScheduleMode;
  maxConcurrency: string;
}

function emptyDraft(timezones: string[]): Draft {
  return {
    name: "",
    kind: "recurring",
    cron: "0 22 * * *",
    startAt: "",
    // The browser's own zone when it is one the backend offers, which it will
    // be — both lists are the IANA database. Guessing right here is worth a
    // line, because the zone is the field whose wrong value is invisible.
    timezone: preferredZone(timezones),
    stopAt: "06:00",
    mode: "sequential",
    maxConcurrency: "2",
  };
}

function preferredZone(timezones: string[]): string {
  const local = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return timezones.includes(local) ? local : (timezones[0] ?? "UTC");
}

function draftOf(schedule: ScheduleView): Draft {
  return {
    name: schedule.name,
    kind: schedule.cron ? "recurring" : "once",
    cron: schedule.cron ?? "0 22 * * *",
    startAt: schedule.startAt ? toDateTimeLocal(schedule.startAt) : "",
    timezone: schedule.timezone ?? "UTC",
    stopAt: schedule.stopAt ?? "",
    mode: schedule.mode,
    maxConcurrency: String(schedule.maxConcurrency),
  };
}

function toInput(draft: Draft): ScheduleInput {
  const parsed = Number(draft.maxConcurrency);
  return {
    name: draft.name,
    mode: draft.mode,
    // A blank or nonsense field becomes 1 rather than `NaN`, which would
    // serialize as `null` and be refused with a message about the wrong field.
    // The backend still holds the real range; this only keeps the wire honest.
    maxConcurrency: Number.isInteger(parsed) && parsed >= 1 ? parsed : 1,
    timezone: draft.timezone,
    cron: draft.kind === "recurring" ? draft.cron : null,
    startAt: draft.kind === "once" && draft.startAt ? new Date(draft.startAt).toISOString() : null,
    stopAt: draft.stopAt === "" ? null : draft.stopAt,
    enabled: true,
  };
}

/** `YYYY-MM-DDTHH:mm` in the browser's local zone, which is what
 *  `<input type="datetime-local">` reads and writes. */
function toDateTimeLocal(iso: string): string {
  const at = new Date(iso);
  const pad = (value: number) => String(value).padStart(2, "0");
  return (
    `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}` +
    `T${pad(at.getHours())}:${pad(at.getMinutes())}`
  );
}

/**
 * The schedule's kind, as a sentence.
 *
 * The nightly shape is spelled out because it is the one this product is named
 * for; **every other expression is shown verbatim**, deliberately. Paraphrasing
 * an arbitrary cron expression would be a second cron implementation in
 * TypeScript, which is exactly what task 013's Notes warn against — and one
 * that disagreed with the backend's would be worse than no prose at all, since
 * the whole point of this row is to be checkable. The `nextFireAt` beside it is
 * the authoritative answer either way.
 */
function describe(schedule: ScheduleView): string {
  const zone = schedule.timezone ?? "no timezone set";
  if (schedule.cron) {
    const nightly = /^(\d{1,2}) (\d{1,2}) \* \* \*$/.exec(schedule.cron.trim());
    const when = nightly
      ? `Every day at ${nightly[2].padStart(2, "0")}:${nightly[1].padStart(2, "0")}`
      : `Repeats: ${schedule.cron}`;
    return `${when} · ${zone}`;
  }
  if (schedule.startAt) {
    return `Once, ${absolute(schedule.startAt)}`;
  }
  return "No time set";
}

function absolute(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** "in 6 hours" / "3 hours ago", from `Intl` rather than from a date library —
 *  seam-contract D6's list is closed, and this is the whole of what a date
 *  library would have been added for. */
function relative(iso: string): string {
  const seconds = Math.round((new Date(iso).getTime() - Date.now()) / 1000);
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const steps: ReadonlyArray<[Intl.RelativeTimeFormatUnit, number]> = [
    ["day", 86_400],
    ["hour", 3_600],
    ["minute", 60],
  ];
  for (const [unit, size] of steps) {
    if (Math.abs(seconds) >= size) {
      return format.format(Math.round(seconds / size), unit);
    }
  }
  return format.format(seconds, "second");
}

/** The same words the board's own badge uses, so a task explained here and a
 *  task explained there cannot disagree. */
function explain(skip: string | null): string {
  switch (skip) {
    case "unattended_runs_not_allowed":
      return "this repository has not enabled unattended agent runs";
    case "dependency_not_satisfied":
      return "waiting on a dependency";
    case "already_in_flight":
      return "already started";
    case "waiting_for_retry":
      return "waiting to resume";
    case "needs_attention":
      return "the last run did not succeed";
    default:
      return "skipped";
  }
}
