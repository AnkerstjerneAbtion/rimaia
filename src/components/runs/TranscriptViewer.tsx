import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import { readRunTranscriptPage, searchRunTranscript, toRimaiaError } from "../../lib/commands";
import type {
  RimaiaError,
  SearchHit,
  TranscriptBlock,
  TranscriptEntry,
  TranscriptPage,
} from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface TranscriptViewerProps {
  readonly runId: string;
}

/**
 * Task 015's transcript viewer: paginated rather than virtualized — see
 * `rimaia_core::runs::transcript`'s own module doc for why. Each page is a
 * bounded read off the backend (`readRunTranscriptPage`), so a 50MB file
 * never has to be held whole in this component's state; only ever
 * {@link PAGE_SIZE} entries are.
 *
 * Rendered only when the run's log is available — the caller
 * (`RunDetailOverlay`) checks `logAvailable` and shows "log unavailable"
 * itself rather than handing this component a path that does not resolve.
 */
const PAGE_SIZE = 100;

export function TranscriptViewer({ runId }: TranscriptViewerProps) {
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<TranscriptPage | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [searching, setSearching] = useState(false);

  useEffect(() => {
    let active = true;
    setPage(null);
    readRunTranscriptPage(runId, offset, PAGE_SIZE).then(
      (result) => {
        if (active) {
          setPage(result);
          setError(null);
        }
      },
      (thrown) => {
        if (active) setError(toRimaiaError(thrown));
      },
    );
    return () => {
      active = false;
    };
  }, [runId, offset]);

  async function handleSearch(event: FormEvent) {
    event.preventDefault();
    if (!query.trim()) {
      setHits(null);
      return;
    }
    setSearching(true);
    try {
      const result = await searchRunTranscript(runId, query);
      setHits(result);
      setError(null);
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setSearching(false);
    }
  }

  // `hit.entry` is already an offset in the same numbering pages use — the
  // backend counts it during the scan that finds the hit, precisely so this
  // never has to convert a file line into one (it cannot: the two differ by
  // however many blank lines precede the match, which only re-reading the
  // file would reveal). Landing the page on the hit rather than centring it
  // is a deliberate simplification: the match sits at the top of the page.
  function jumpToHit(hit: SearchHit) {
    setHits(null);
    setOffset(hit.entry);
  }

  return (
    <div className="transcript-viewer">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      <form className="transcript-search" onSubmit={handleSearch}>
        <input
          type="text"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search this transcript…"
          aria-label="Search this transcript"
        />
        <button type="submit" disabled={searching}>
          {searching ? "Searching…" : "Search"}
        </button>
      </form>

      {hits && (
        <ul className="transcript-search-results">
          {hits.length === 0 && <li className="muted">No matches.</li>}
          {hits.map((hit) => (
            <li key={hit.line}>
              <button type="button" onClick={() => jumpToHit(hit)}>
                Line {hit.line}: <span className="transcript-snippet">{hit.snippet}</span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {page === null && !error && <p className="muted">Loading transcript…</p>}

      {page && (
        <>
          <div className="transcript-entries">
            {page.entries.length === 0 && <p className="muted">This page has nothing to show.</p>}
            {page.entries.map((entry) => (
              <TranscriptEntryView key={entry.line} entry={entry} />
            ))}
          </div>

          <div className="transcript-pagination">
            <button
              type="button"
              disabled={offset === 0}
              onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
            >
              Previous
            </button>
            <span className="muted">
              {page.totalLines === 0
                ? "Empty transcript"
                : `${offset + 1}–${Math.min(offset + page.entries.length, page.totalLines)} of ${page.totalLines}`}
            </span>
            <button
              type="button"
              disabled={offset + page.entries.length >= page.totalLines}
              onClick={() => setOffset(offset + PAGE_SIZE)}
            >
              Next
            </button>
          </div>
        </>
      )}
    </div>
  );
}

/** The speaker, in a gutter of its own rather than woven into the text.
 *  Reading a transcript is mostly *skipping* — five rems of uppercase eyebrow
 *  down the left edge is what lets somebody skip every tool result in a
 *  thousand-entry file without reading one. */
function TranscriptRole({ children }: { readonly children: string }) {
  return <span className="transcript-role">{children}</span>;
}

function TranscriptEntryView({ entry }: { readonly entry: TranscriptEntry }) {
  const kind = entry.kind;
  switch (kind.type) {
    case "assistant":
      return (
        <div className="transcript-entry transcript-assistant">
          <TranscriptRole>Agent</TranscriptRole>
          {kind.blocks.map((block, index) => (
            <TranscriptBlockView key={index} block={block} />
          ))}
        </div>
      );
    case "user":
      return (
        <div className="transcript-entry transcript-user">
          <TranscriptRole>Input</TranscriptRole>
          {kind.blocks.map((block, index) => (
            <TranscriptBlockView key={index} block={block} />
          ))}
        </div>
      );
    case "result":
      return (
        <div
          className={
            kind.isError
              ? "transcript-entry transcript-result transcript-error"
              : "transcript-entry transcript-result"
          }
        >
          <TranscriptRole>Result</TranscriptRole>
          {kind.summary && <p className="transcript-text">{kind.summary}</p>}
          {kind.errors.map((text, index) => (
            <p key={index} className="transcript-error-text">
              {text}
            </p>
          ))}
        </div>
      );
    case "other":
      // With the subtype, because without it a real transcript is mostly this
      // one label: `system` covers the run's own init, hook results, subagent
      // lifecycle and a per-tick token counter, and they are not the same
      // event in any sense a reader cares about.
      return (
        <p className="muted transcript-other">
          Unrendered event: {kind.subtype ? `${kind.eventType}/${kind.subtype}` : kind.eventType}
        </p>
      );
    case "malformed":
      // The text, not just the fact. A trailing unparseable line is a stream
      // cut mid-write, and what it was cut in the middle of is usually the
      // agent's last message — the one thing worth reading on a run that
      // ended without saying why.
      return (
        <div className="transcript-entry transcript-malformed">
          <TranscriptRole>Raw</TranscriptRole>
          <p className="transcript-error-text">
            Line {entry.line}: not valid JSON — shown as written.
          </p>
          <pre className="transcript-malformed-raw">{kind.raw}</pre>
        </div>
      );
  }
}

function TranscriptBlockView({ block }: { readonly block: TranscriptBlock }) {
  switch (block.kind) {
    case "text":
      return <p className="transcript-text">{block.text}</p>;
    case "tool_use":
      return (
        <div className="transcript-tool-use">
          <span className="transcript-tool-name">{block.name}</span>
          <pre className="transcript-tool-input">{JSON.stringify(block.input, null, 2)}</pre>
        </div>
      );
    case "tool_result":
      return (
        <details
          className={
            block.isError
              ? "transcript-tool-result transcript-error"
              : "transcript-tool-result"
          }
        >
          <summary>{block.isError ? "Tool result (error)" : "Tool result"}</summary>
          <pre>{block.content ?? "(no content)"}</pre>
        </details>
      );
    case "other":
      return null;
  }
}
