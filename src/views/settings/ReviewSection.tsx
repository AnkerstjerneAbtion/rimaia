import { useEffect, useRef, useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { ReviewConfigFields } from "../../components/ReviewConfigFields";
import { useReviewLevel } from "../../hooks/useReviewLevel";
import {
  getReviewSettings,
  getStrategyCatalogue,
  setReviewSettings,
  toRimaiaError,
} from "../../lib/commands";
import type { Catalogue, ReviewConfig, RimaiaError } from "../../types";

/**
 * Settings → Review (task 037, ADR-0017): the global half of the review loop's
 * configuration, and the instructions every review run is given.
 *
 * Rimaia ships no review methodology. The instructions are often just the name
 * of the operator's own review skill or slash command, and the loop's whole
 * quality is theirs; this section's job is to make that one text box and the
 * handful of numbers around it easy to find.
 *
 * Both halves are written by one command, `set_review_settings`, which replaces
 * the instructions and the configuration together. So each save reads the
 * other half fresh first: another window, or an MCP client, is a supported
 * writer of the same keys (ADR-0006), and writing back what this form read a
 * minute ago would undo whatever they did since.
 */
export function ReviewSection() {
  const level = useReviewLevel("global", null);
  const [instructions, setInstructions] = useState<string | null>(null);
  const [catalogue, setCatalogue] = useState<Catalogue | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    getReviewSettings().then(
      (settings) => setInstructions(settings.instructions),
      (thrown) => setError(toRimaiaError(thrown)),
    );
    // The model and effort vocabulary (D17); the fields fall back to inherit
    // alone while it loads, and a failure here costs only the two dropdowns.
    getStrategyCatalogue().then(
      (view) => setCatalogue(view.catalogue),
      () => {},
    );
  }, []);

  async function saveConfig(config: ReviewConfig) {
    const current = await getReviewSettings();
    await setReviewSettings(current.instructions, config);
    await level.reload();
  }

  return (
    <section className="panel review-section">
      <h3>Review</h3>
      <p className="muted">
        After a task is implemented, a fresh agent can review it, fix what it finds and review
        again, up to a bound. Each pass is another run on your subscription. Repositories and
        single tasks can override any of this; the findings and what became of them are on the
        task.
      </p>

      {(error || level.error) && (
        <ErrorBanner
          error={(error ?? level.error) as RimaiaError}
          onDismiss={() => {
            setError(null);
            level.dismissError();
          }}
        />
      )}

      <div className="instructions-subsection">
        <h4>Review instructions</h4>
        <p className="muted">
          What every review run is told to do. Often just the name of your own review skill or
          slash command. Rimaia ships no methodology of its own.
        </p>
        {instructions === null ? (
          !error && <p className="muted">Reading…</p>
        ) : (
          <ReviewInstructionsEditor initialValue={instructions} />
        )}
      </div>

      <div className="instructions-subsection">
        <h4>Loop</h4>
        {level.data === null ? (
          !level.error && <p className="muted">Reading…</p>
        ) : (
          <ReviewConfigFields
            scope="global"
            idPrefix="review-global"
            level={level.data}
            catalogue={catalogue}
            onChange={saveConfig}
          />
        )}
      </div>
    </section>
  );
}

/**
 * Same uncontrolled, commit-on-blur-or-unmount shape as the base instructions
 * editor beside it, for the reason that one gives: React 19 detaches refs
 * before effect cleanups run, so the draft is mirrored out of the DOM per
 * keystroke.
 */
function ReviewInstructionsEditor({ initialValue }: { readonly initialValue: string }) {
  const draftRef = useRef(initialValue);
  const lastSavedRef = useRef(initialValue);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  async function save(value: string) {
    const current = await getReviewSettings();
    await setReviewSettings(value, current.config);
  }

  function commit(value: string) {
    draftRef.current = value;
    if (value === lastSavedRef.current) return;
    setSaving(true);
    setError(null);
    save(value).then(
      () => {
        lastSavedRef.current = value;
        setSaving(false);
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  useEffect(() => {
    return () => {
      if (draftRef.current !== lastSavedRef.current) {
        save(draftRef.current).catch(() => {});
      }
    };
  }, []);

  return (
    <>
      {saving && <span className="muted">Saving…</span>}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <textarea
        className="instructions-editor-textarea"
        defaultValue={initialValue}
        onChange={(event) => {
          draftRef.current = event.target.value;
        }}
        onBlur={(event) => commit(event.target.value)}
        aria-label="Review instructions"
        placeholder="For example: run /review and report what it finds."
      />
    </>
  );
}
