import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import {
  acceptTaskStrategy,
  createPairingCode,
  createPersonalAccessToken,
  getAccount,
  listApiTokens,
  listSessions,
  pairOwnRunner,
  revokeApiToken,
  revokeSession,
  approveTask,
  getReviewDigest,
  getAppInfo,
  getClientCapabilities,
  getTask,
  getTaskDependents,
  createTask,
  deleteTask,
  listRepositories,
  runDoctor,
  setCommandTransports,
  httpCommandTransport,
  markReviewDigestSeen,
  PROTOCOL_VERSION,
  setCommandTransport,
  rejectTask,
  requestTaskChanges,
  clearTaskStrategy,
  getStrategyApproval,
  getStrategyCatalogue,
  getStrategyDefaults,
  giveUpOnTask,
  planTaskStrategy,
  retryTaskNow,
  setStrategyApproval,
  setStrategyCatalogue,
  setStrategyDefaults,
  startRun,
  toRimaiaError,
} from "./commands";
import { installTransports } from "./client";
import { setEventTransports } from "./events";
import { mockHttp } from "../test/http";
import type { StrategyCatalogueView, StrategyDefaults } from "../types";

// Mocked at the Tauri seam, not at the wrapper module — the wrappers are what
// is under test here, and mocking them would leave the command names and
// argument keys they send untested. Those names, and whether each wrapper calls
// `board` or `local`, are exactly what `scripts/check-command-wiring.sh`
// cross-checks against the registry, and the argument keys are the half of the
// contract no script can see.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

describe("toRimaiaError", () => {
  it("passes a backend {code, message} payload through unchanged", () => {
    const thrown = { code: "not_found", message: "repository not found" };

    expect(toRimaiaError(thrown)).toEqual({
      code: "not_found",
      message: "repository not found",
    });
  });

  it("wraps a bare string rejection as an internal error", () => {
    expect(toRimaiaError("permission denied")).toEqual({
      code: "internal",
      message: "permission denied",
    });
  });

  it("uses a JS Error's message as an internal error", () => {
    expect(toRimaiaError(new Error("network unreachable"))).toEqual({
      code: "internal",
      message: "network unreachable",
    });
  });

  it("renders a non-conforming object as readable JSON, not [object Object]", () => {
    const result = toRimaiaError({ unexpected: "shape" });

    expect(result.code).toBe("internal");
    expect(result.message).not.toBe("[object Object]");
    expect(result.message).toBe('{"unexpected":"shape"}');
  });
});

describe("the execution-strategy wrappers (task 020)", () => {
  const catalogue: StrategyCatalogueView = {
    catalogue: {
      models: [{ id: "opus", label: "Opus" }],
      efforts: [{ id: "low", label: "Low" }],
      planner: { model: "haiku", effort: "low", max_turns: 6 },
    },
    json: '{ "models": [{ "id": "opus", "label": "Opus" }] }',
    defaultJson: '{ "models": [] }',
    providerInfo: { id: "claude-code", displayName: "Claude Code" },
  };

  const defaults: StrategyDefaults = {
    mode: "manual",
    model: "sonnet",
    effort: "high",
  };

  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("reads the catalogue with no arguments at all", async () => {
    mockInvoke.mockResolvedValue(catalogue);

    await expect(getStrategyCatalogue()).resolves.toEqual(catalogue);
    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_catalogue", undefined);
  });

  it("sends an edited catalogue as text and answers with the stored view", async () => {
    // The text is what crosses, not a re-serialized object: the backend stores
    // the user's own key order and indentation, and the view it answers with is
    // what the editor reopens on.
    mockInvoke.mockResolvedValue(catalogue);

    await expect(setStrategyCatalogue(catalogue.json)).resolves.toEqual(catalogue);
    expect(mockInvoke).toHaveBeenCalledWith("set_strategy_catalogue", {
      value: catalogue.json,
    });
  });

  it("asks for the global defaults when no repository is named", async () => {
    mockInvoke.mockResolvedValue(defaults);

    await expect(getStrategyDefaults()).resolves.toEqual(defaults);
    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_defaults", {
      repositoryId: null,
    });
  });

  it("asks for one repository's defaults when it is named", async () => {
    mockInvoke.mockResolvedValue(defaults);

    await getStrategyDefaults("repo-1");

    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_defaults", {
      repositoryId: "repo-1",
    });
  });

  it("writes the global defaults and one repository's through the same command", async () => {
    // One command for both levels, because one struct and one precedence chain
    // read both — the repository id is the only thing that differs.
    mockInvoke.mockResolvedValue(undefined);

    await setStrategyDefaults(null, defaults);
    await setStrategyDefaults("repo-1", defaults);

    expect(mockInvoke).toHaveBeenNthCalledWith(1, "set_strategy_defaults", {
      repositoryId: null,
      value: defaults,
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, "set_strategy_defaults", {
      repositoryId: "repo-1",
      value: defaults,
    });
  });

  it("reads and writes the approval setting", async () => {
    mockInvoke.mockResolvedValue("manual");

    await expect(getStrategyApproval()).resolves.toBe("manual");
    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_approval", undefined);

    mockInvoke.mockResolvedValue(undefined);
    await setStrategyApproval("manual");

    expect(mockInvoke).toHaveBeenLastCalledWith("set_strategy_approval", {
      value: "manual",
    });
  });

  it("accepts and clears a proposal by task id", async () => {
    mockInvoke.mockResolvedValue(undefined);

    await acceptTaskStrategy("task-1");
    await clearTaskStrategy("task-1");

    expect(mockInvoke).toHaveBeenNthCalledWith(1, "accept_task_strategy", {
      taskId: "task-1",
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, "clear_task_strategy", {
      taskId: "task-1",
    });
  });

  it("starts a planner run by task id", async () => {
    mockInvoke.mockResolvedValue(undefined);

    await planTaskStrategy("task-1");

    expect(mockInvoke).toHaveBeenCalledWith("plan_task_strategy", {
      taskId: "task-1",
    });
  });

  it("rejects with a renderable RimaiaError when a strategy command is refused", async () => {
    // Every wrapper goes through the same `send`, board and local alike, so one
    // of them is enough to
    // pin that a refusal arrives as `{code, message}` and not as whatever
    // `invoke` happened to throw.
    mockInvoke.mockRejectedValue({
      code: "invalid",
      message: "the catalogue is not valid JSON: key must be a string at line 1 column 3",
    });

    await expect(setStrategyCatalogue("{ models: [] }")).rejects.toEqual({
      code: "invalid",
      message: "the catalogue is not valid JSON: key must be a string at line 1 column 3",
    });
  });
});

describe("Run now names its runner only when given one (task 052)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue(undefined);
  });

  it("sends the task alone when no runner is named", async () => {
    await startRun("task-1");
    expect(mockInvoke).toHaveBeenCalledWith("start_task_run", { taskId: "task-1" });
  });

  it("sends the runner when one is named", async () => {
    await startRun("task-1", "runner-1");
    expect(mockInvoke).toHaveBeenCalledWith("start_task_run", {
      taskId: "task-1",
      runnerId: "runner-1",
    });
  });
});

describe("the retry controls (task 014)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("resumes a waiting task under the name the command is registered as", () => {
    // The name matters more than usual here: `check-command-wiring.sh` reads
    // this literal out of the source to prove the command is registered, so a
    // template string would make the wrapper invisible to it *and* to the user,
    // as a command-not-found at 09:00.
    mockInvoke.mockResolvedValue(undefined);

    return retryTaskNow("task-1").then(() => {
      expect(mockInvoke).toHaveBeenCalledWith("retry_task_now", { taskId: "task-1" });
    });
  });

  it("gives up on a waiting task", () => {
    mockInvoke.mockResolvedValue(undefined);

    return giveUpOnTask("task-1").then(() => {
      expect(mockInvoke).toHaveBeenCalledWith("give_up_on_task", { taskId: "task-1" });
    });
  });

  it("surfaces a refusal as a readable error rather than an object", () => {
    // "Give up" on a task that is not waiting is a sentence about the card, and
    // this is the seam that keeps it one.
    mockInvoke.mockRejectedValue({
      code: "invalid",
      message: "this task is not waiting to be retried (it is running)",
    });

    return giveUpOnTask("task-1").then(
      () => expect.fail("a refusal must reject"),
      (thrown) => {
        const error = toRimaiaError(thrown);
        expect(error.code).toBe("invalid");
        expect(error.message).toContain("not waiting");
      },
    );
  });
});

describe("the review wrappers (task 034)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue(undefined);
  });

  it("sends each verdict with the argument names the Rust commands take", async () => {
    await approveTask("task-1");
    await rejectTask("task-1", "Start over.");
    await requestTaskChanges("task-1", "Add a test.");

    expect(mockInvoke).toHaveBeenNthCalledWith(1, "approve_task", { taskId: "task-1" });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, "reject_task", {
      taskId: "task-1",
      note: "Start over.",
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(3, "request_task_changes", {
      taskId: "task-1",
      note: "Add a test.",
    });
  });

  it("sends the two reads and the marker write", async () => {
    await getTaskDependents("task-1");
    await getReviewDigest();
    await markReviewDigestSeen("2026-10-04T12:00:00.000Z");

    expect(mockInvoke).toHaveBeenNthCalledWith(1, "get_task_dependents", { taskId: "task-1" });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, "get_review_digest", undefined);
    expect(mockInvoke).toHaveBeenNthCalledWith(3, "mark_review_digest_seen", {
      through: "2026-10-04T12:00:00.000Z",
    });
  });
});

describe("board and local (seam-contract D32 point 6)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    setCommandTransport(undefined);
  });

  it("sends a board and a local command through the same invoke, unchanged", async () => {
    mockInvoke.mockResolvedValue(undefined);

    await getTaskDependents("task-1");
    await getAppInfo();

    expect(mockInvoke).toHaveBeenNthCalledWith(1, "get_task_dependents", { taskId: "task-1" });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, "get_app_info", undefined);
  });

  it("sends both through an installed transport, as fixture mode does", async () => {
    const transport = vi.fn().mockResolvedValue(null);
    setCommandTransport(transport);

    await getTaskDependents("task-1");
    await getAppInfo();

    expect(transport.mock.calls).toEqual([
      ["get_task_dependents", { taskId: "task-1" }],
      ["get_app_info", undefined],
    ]);
    expect(mockInvoke).not.toHaveBeenCalled();
    setCommandTransport(undefined);
  });

  it("carries the protocol version the server speaks", () => {
    expect(PROTOCOL_VERSION).toBe("1.0");
  });
});

describe("the account wrappers (task 047)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue(undefined);
  });

  it("sends the three reads with no arguments at all", async () => {
    await getAccount();
    await listSessions();
    await listApiTokens();

    expect(mockInvoke.mock.calls).toEqual([
      ["get_account", undefined],
      ["list_sessions", undefined],
      ["list_api_tokens", undefined],
    ]);
  });

  it("revokes a session and a token by id", async () => {
    await revokeSession("session-1");
    await revokeApiToken("token-1");

    expect(mockInvoke.mock.calls).toEqual([
      ["revoke_session", { id: "session-1" }],
      ["revoke_api_token", { id: "token-1" }],
    ]);
  });

  it("sends a personal access token's label, teams and expiry under the Rust names", async () => {
    await createPersonalAccessToken("Claude Code", ["team-1"], 30);
    await createPersonalAccessToken("Everything");

    expect(mockInvoke.mock.calls).toEqual([
      [
        "create_personal_access_token",
        { label: "Claude Code", teamIds: ["team-1"], expiresInDays: 30 },
      ],
      [
        "create_personal_access_token",
        { label: "Everything", teamIds: undefined, expiresInDays: undefined },
      ],
    ]);
  });

  it("asks for a pairing code and pairs the desktop's own runner", async () => {
    await createPairingCode();
    await pairOwnRunner("This Mac", "claude-code");

    expect(mockInvoke.mock.calls).toEqual([
      ["create_pairing_code", undefined],
      ["pair_own_runner", { label: "This Mac", provider: "claude-code" }],
    ]);
  });
});

describe("the HTTP command transport (task 049)", () => {
  const SERVER = "https://board.example";
  const session = { kind: "session", csrf: () => "csrf-1" } as const;

  function installHttp(credential: Parameters<typeof httpCommandTransport>[0]["credential"]) {
    setCommandTransports({
      board: httpCommandTransport({ baseUrl: SERVER, credential }),
      local: (command) => Promise.reject({ code: "invalid", message: `${command} is local` }),
    });
  }

  afterEach(() => {
    setCommandTransports(undefined);
    setEventTransports(undefined);
    vi.unstubAllGlobals();
    document.cookie = "rimaia_csrf=; expires=Thu, 01 Jan 1970 00:00:00 GMT";
    mockInvoke.mockReset();
  });

  it("posts a board command to /api/v1/<name> with the invoke arguments as its body", async () => {
    const http = mockHttp();
    http.answer("get_task", { id: "task-1" });
    http.answer("list_repositories", []);
    installHttp(session);

    await getTask("task-1");
    await listRepositories();

    expect(http.requests.map((r) => [r.method, r.url, r.body])).toEqual([
      ["POST", `${SERVER}/api/v1/get_task`, { id: "task-1" }],
      ["POST", `${SERVER}/api/v1/list_repositories`, {}],
    ]);
    expect(JSON.stringify({ id: "task-1" })).toBe('{"id":"task-1"}');
  });

  it("sends Rimaia-Protocol with the PROTOCOL_VERSION literal on every request", async () => {
    const http = mockHttp();
    http.answer("list_repositories", []);
    installHttp(session);

    await listRepositories();
    await listRepositories();

    expect(http.requests.map((r) => r.headers["rimaia-protocol"])).toEqual([
      PROTOCOL_VERSION,
      PROTOCOL_VERSION,
    ]);
    expect(http.requests[0].headers["content-type"]).toBe("application/json");
  });

  it("sends the CSRF header and no Authorization header with a session credential", async () => {
    const http = mockHttp();
    http.answer("list_repositories", []);
    installHttp(session);

    await listRepositories();

    const [request] = http.requests;
    expect(request.headers["x-rimaia-csrf"]).toBe("csrf-1");
    expect(request.headers).not.toHaveProperty("authorization");
    expect(request.credentials).toBe("same-origin");
  });

  it("reads the CSRF header from the rimaia_csrf cookie on every request", async () => {
    const http = mockHttp();
    http.answer("list_repositories", []);
    installTransports("browser");

    document.cookie = "rimaia_csrf=first";
    await listRepositories();
    document.cookie = "rimaia_csrf=second";
    await listRepositories();
    document.cookie = "rimaia_csrf=; expires=Thu, 01 Jan 1970 00:00:00 GMT";
    await listRepositories();

    expect(http.requests.map((r) => r.headers["x-rimaia-csrf"])).toEqual([
      "first",
      "second",
      undefined,
    ]);
    expect(http.requests[2].headers).not.toHaveProperty("x-rimaia-csrf");
  });

  it("sends a bearer token, omits cookies and sends no CSRF header with a bearer credential", async () => {
    const http = mockHttp();
    http.answer("list_repositories", []);
    installHttp({ kind: "bearer", token: "rmd_secret" });

    await listRepositories();

    const [request] = http.requests;
    expect(request.headers.authorization).toBe("Bearer rmd_secret");
    expect(request.headers).not.toHaveProperty("x-rimaia-csrf");
    expect(request.credentials).toBe("omit");
  });

  it("resolves a null answer as void", async () => {
    const http = mockHttp();
    http.answer("delete_task", null);
    installHttp(session);

    await expect(deleteTask("task-1")).resolves.toBeNull();
  });

  it("turns a 200 without a JSON body into an internal error", async () => {
    const http = mockHttp();
    http.refuseRaw("list_repositories", 200, "<html>fallback</html>");
    installHttp(session);

    await expect(listRepositories()).rejects.toEqual({
      code: "internal",
      message: "The server answered HTTP 200 without a JSON body.",
    });
  });

  it.each([
    [400, "invalid"],
    [401, "unauthenticated"],
    [404, "not_found"],
    [409, "conflict"],
    [426, "upgrade_required"],
    [500, "internal"],
  ])("rejects with the server's code and message unchanged for %i", async (status, code) => {
    const http = mockHttp();
    http.refuse("get_task", status, { code, message: `the server said ${code}` });
    installHttp(session);

    await expect(getTask("task-1")).rejects.toEqual({ code, message: `the server said ${code}` });
  });

  it("turns an error body that is not JSON into an internal error naming the status", async () => {
    const http = mockHttp();
    http.refuseRaw("get_task", 502, "<html>Bad Gateway</html>");
    installHttp(session);

    await expect(getTask("task-1")).rejects.toEqual({
      code: "internal",
      message: "The server answered HTTP 502 without an error body.",
    });
  });

  it("turns a fetch that throws into an internal error naming the server", async () => {
    const http = mockHttp();
    http.unreachable("Failed to fetch");
    installHttp(session);

    await expect(getTask("task-1")).rejects.toEqual({
      code: "internal",
      message: `Could not reach the server at ${SERVER}: Failed to fetch.`,
    });
  });

  it("gives a wrapper the same answer and the same error through invoke and HTTP", async () => {
    const repositories = [{ id: "repo-1", name: "rimaia", defaultBranch: "main" }];
    const created = { id: "task-9", title: "A task" };
    const refusal = { code: "not_found", message: "task not found" };
    const input = { repositoryId: "repo-1", title: "A task" };

    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_repositories") return repositories;
      if (command === "create_task") return created;
      throw refusal;
    });
    const throughInvoke = [
      await listRepositories(),
      await createTask(input),
      await getTask("nope").catch((error: unknown) => error),
    ];

    const http = mockHttp();
    http.answer("list_repositories", repositories);
    http.answer("create_task", created);
    http.refuse("get_task", 404, refusal);
    installHttp(session);
    const throughHttp = [
      await listRepositories(),
      await createTask(input),
      await getTask("nope").catch((error: unknown) => error),
    ];

    expect(throughHttp).toEqual(throughInvoke);
    expect(http.requests[1].body).toEqual({ input });
  });
});

describe("the transports by command kind (task 049)", () => {
  afterEach(() => {
    setCommandTransports(undefined);
    setEventTransports(undefined);
    vi.unstubAllGlobals();
    mockInvoke.mockReset();
  });

  it("sends every command through invoke when nothing is installed", async () => {
    mockInvoke.mockResolvedValue([]);

    await listRepositories();
    await getAppInfo();

    expect(mockInvoke.mock.calls.map(([command]) => command)).toEqual([
      "list_repositories",
      "get_app_info",
    ]);
  });

  it("routes board commands to HTTP and local commands to invoke in connected mode", async () => {
    const http = mockHttp();
    http.answer("list_repositories", []);
    mockInvoke.mockResolvedValue({ appVersion: "1.2.3" });
    installTransports("connected", { serverUrl: "https://board.example", token: "rmd_t" });

    await listRepositories();
    await getAppInfo();

    expect(http.requests.map((r) => r.name)).toEqual(["list_repositories"]);
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("get_app_info", undefined);
  });

  it("refuses a local command in the browser without making a request", async () => {
    const http = mockHttp();
    installTransports("browser");

    await expect(runDoctor()).rejects.toEqual({
      code: "invalid",
      message: "run_doctor is only available in the desktop app.",
    });

    expect(http.requests).toEqual([]);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("answers get_client_capabilities in the browser without a request", async () => {
    const http = mockHttp();
    installTransports("browser");

    await expect(getClientCapabilities()).resolves.toEqual({
      mode: "browser",
      localRunnerId: null,
      canOpenInEditor: false,
      canRevealFiles: false,
      canChooseFolders: false,
    });

    expect(http.requests).toEqual([]);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("still sends both kinds through a transport installed with setCommandTransport", async () => {
    const transport = vi.fn().mockResolvedValue([]);
    setCommandTransport(transport);

    await listRepositories();
    await getAppInfo();

    expect(transport.mock.calls.map(([command]) => command)).toEqual([
      "list_repositories",
      "get_app_info",
    ]);
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});
