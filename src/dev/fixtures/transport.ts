import type { CommandTransport } from "../../lib/commands";
import type { EventTransport } from "../../lib/events";
import type { RunTail } from "../../types";
import { ANSWERS, refuse } from "./answers";
import { SETTLED_FLAG } from "./constants";
import type { Scenario } from "./seed";

/**
 * The fixture's two halves of the transport seam, sharing one count of
 * outstanding work.
 *
 * "Outstanding" is a command answer not yet resolved or a canned event not
 * yet delivered to a live subscriber. `window.__rimaiaFixtureSettled` goes
 * `false` synchronously the moment work arrives and `true` only once the count
 * has stayed at zero across a few macrotasks — React flushes the state a
 * resolved command set in a task of its own, and the effects that follow may
 * ask for more, so a single `setTimeout(0)` can fire between two reads. The
 * screenshot script waits on the flag instead of a fixed delay.
 */

/** Macrotasks the count must stay at zero for before the page counts as settled. */
const QUIET_TASKS = 3;

type SettledWindow = Window & { [SETTLED_FLAG]?: boolean };

export interface FixtureTransports {
  readonly command: CommandTransport;
  readonly event: EventTransport;
}

export function createFixtureTransports(scenario: Scenario): FixtureTransports {
  let outstanding = 0;
  let generation = 0;

  function setFlag(value: boolean) {
    if (typeof window !== "undefined") (window as SettledWindow)[SETTLED_FLAG] = value;
  }

  function begin() {
    outstanding += 1;
    generation += 1;
    setFlag(false);
  }

  function end() {
    outstanding -= 1;
    if (outstanding > 0) return;
    const mine = generation;
    let remaining = QUIET_TASKS;
    const tick = () => {
      if (outstanding > 0 || generation !== mine) return;
      remaining -= 1;
      if (remaining === 0) setFlag(true);
      else setTimeout(tick, 0);
    };
    setTimeout(tick, 0);
  }

  setFlag(false);

  const command: CommandTransport = (name, args) => {
    begin();
    // Resolved on a macrotask, never inline: a fixture that answered
    // synchronously would hide every "loading" state the real backend shows.
    return new Promise((resolve, reject) => {
      setTimeout(() => {
        try {
          const answer = ANSWERS[name];
          if (!answer) {
            refuse("internal", `fixture mode has no answer for \`${name}\``);
          }
          resolve(answer(args ?? {}, scenario));
        } catch (thrown) {
          reject(thrown);
        } finally {
          end();
        }
      }, 0);
    });
  };

  const tailSubscribers = new Set<(tail: RunTail) => void>();

  const event: EventTransport = <P>(name: string, onPayload: (payload: P) => void) => {
    if (name !== "runs:tail") {
      // Nothing in a fixture ever changes, so no other channel ever fires.
      return Promise.resolve(() => {});
    }
    const subscriber = onPayload as unknown as (tail: RunTail) => void;
    tailSubscribers.add(subscriber);
    begin();
    setTimeout(() => {
      // Only if still subscribed: a card that unmounted in between has nothing
      // to hear it.
      if (tailSubscribers.has(subscriber)) {
        for (const tail of scenario.tails) subscriber(tail);
      }
      end();
    }, 0);
    return Promise.resolve(() => {
      tailSubscribers.delete(subscriber);
    });
  };

  return { command, event };
}
