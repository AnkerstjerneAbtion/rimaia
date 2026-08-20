import { EmptyState } from "../components/EmptyState";

export function RunsView() {
  return (
    <EmptyState
      title="Nothing has run yet"
      body="Each run lands here with its diff, its commits, the PR link if one was opened, and the full transcript underneath."
      arrivesIn="Runs are produced by task 008 and reviewed in task 015."
    />
  );
}
