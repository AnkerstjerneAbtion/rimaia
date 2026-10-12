import { vi } from "vitest";

/**
 * A `fetch` for the test suite, installed with `vi.stubGlobal` beside the
 * `@tauri-apps/api/core` mocks (task 049, D34 declines `msw`). It replaces
 * `fetch` at the boundary where the suite already mocks `invoke`, so the
 * transports under test are the real ones.
 *
 * Teardown is `vi.unstubAllGlobals()` in the test file's `afterEach`, the way
 * the files that mock `invoke` reset `mockInvoke`. Nothing here mocks
 * `commands.ts` or `events.ts`: a test goes through the wrappers, as a
 * component does.
 */

/** One command the transport sent, with its body parsed. */
export interface RecordedRequest {
  name: string;
  url: string;
  method: string;
  /** Header names lower-cased, because HTTP header names are case-insensitive. */
  headers: Record<string, string>;
  credentials: RequestCredentials | undefined;
  body: unknown;
}

/** The controllable stream `GET /api/v1/events` answers with. */
export interface MockEventStream {
  /** Writes one frame to the stream that is open now. */
  send(event: string, payload: unknown): void;
  /** Closes the current body cleanly: the library's `onclose`. */
  end(): void;
  /** Errors the current body: the library's `onerror`. */
  fail(): void;
  /** Answers the next open with this status and body instead of streaming. */
  failOpen(status: number, body: unknown, contentType?: string): void;
  /** Every open, in order. */
  readonly opens: RecordedRequest[];
  /** True while a stream is live. */
  readonly open: boolean;
}

export interface HttpMock {
  answer(name: string, value: unknown | ((args: unknown) => unknown)): void;
  refuse(name: string, status: number, error: { code: string; message: string }): void;
  refuseRaw(name: string, status: number, body: string): void;
  /** Makes every `fetch` throw, as DNS, a refused connection or CORS would. */
  unreachable(message: string): void;
  readonly requests: RecordedRequest[];
  events(): MockEventStream;
}

type Route = (args: unknown) => Response;

const JSON_TYPE = { "content-type": "application/json" };

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body ?? null), { status, headers: JSON_TYPE });
}

function lowerCased(headers: HeadersInit | undefined): Record<string, string> {
  const recorded: Record<string, string> = {};
  new Headers(headers).forEach((value, name) => {
    recorded[name.toLowerCase()] = value;
  });
  return recorded;
}

export function mockHttp(): HttpMock {
  const routes = new Map<string, Route>();
  const requests: RecordedRequest[] = [];
  let unreachableMessage: string | null = null;

  // The stream.
  const opens: RecordedRequest[] = [];
  let body: ReadableStreamDefaultController<Uint8Array> | null = null;
  let nextOpen: Response | null = null;
  const encoder = new TextEncoder();

  function record(input: RequestInfo | URL, init: RequestInit | undefined): RecordedRequest {
    const url = new URL(typeof input === "string" ? input : String(input), "http://localhost");
    const name = url.pathname.replace(/^\/api\/v1\//, "");
    const text = typeof init?.body === "string" ? init.body : undefined;
    return {
      name,
      url: typeof input === "string" ? input : String(input),
      method: init?.method ?? "GET",
      headers: lowerCased(init?.headers),
      credentials: init?.credentials,
      body: text === undefined ? undefined : JSON.parse(text),
    };
  }

  function openStream(request: RecordedRequest, signal: AbortSignal | null | undefined): Response {
    opens.push(request);
    if (nextOpen) {
      const refused = nextOpen;
      nextOpen = null;
      return refused;
    }
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        body = controller;
        signal?.addEventListener("abort", () => {
          if (body === controller) body = null;
          try {
            controller.error(new DOMException("The operation was aborted.", "AbortError"));
          } catch {
            // Already closed or errored.
          }
        });
      },
    });
    return new Response(stream, {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  }

  const fetchStub = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    if (unreachableMessage !== null) throw new TypeError(unreachableMessage);
    const request = record(input, init);
    if (request.name === "events" && request.method === "GET") {
      return openStream(request, init?.signal);
    }
    requests.push(request);
    const route = routes.get(request.name);
    if (!route) {
      return jsonResponse(404, {
        code: "not_found",
        message: `the mock has no route for ${request.name}`,
      });
    }
    return route(request.body);
  };

  vi.stubGlobal("fetch", vi.fn(fetchStub));

  const stream: MockEventStream = {
    send(event, payload) {
      body?.enqueue(encoder.encode(`event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`));
    },
    end() {
      const current = body;
      body = null;
      current?.close();
    },
    fail() {
      const current = body;
      body = null;
      current?.error(new TypeError("network error"));
    },
    failOpen(status, answer, contentType = "application/json") {
      nextOpen = new Response(typeof answer === "string" ? answer : JSON.stringify(answer), {
        status,
        headers: { "content-type": contentType },
      });
    },
    opens,
    get open() {
      return body !== null;
    },
  };

  return {
    answer(name, value) {
      routes.set(name, (args) =>
        jsonResponse(200, typeof value === "function" ? (value as (a: unknown) => unknown)(args) : value),
      );
    },
    refuse(name, status, error) {
      routes.set(name, () => jsonResponse(status, error));
    },
    refuseRaw(name, status, answer) {
      routes.set(name, () => new Response(answer, { status, headers: { "content-type": "text/html" } }));
    },
    unreachable(message) {
      unreachableMessage = message;
    },
    requests,
    events: () => stream,
  };
}
