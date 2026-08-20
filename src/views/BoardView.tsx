import { EmptyState } from "../components/EmptyState";

export function BoardView() {
  return (
    <EmptyState
      title="No tasks yet"
      body="The board is where plans queue up. Cards move Backlog → Ready → Running → Done, and their order in a column is their priority."
      arrivesIn="The board itself arrives in task 005."
    />
  );
}
