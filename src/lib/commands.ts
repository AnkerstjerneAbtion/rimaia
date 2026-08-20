import { invoke } from "@tauri-apps/api/core";

import type { AppInfo, RimaiaError } from "../types";

/**
 * The only module in the frontend that imports `invoke`.
 *
 * Every backend call goes through `call`, so the serialization boundary has one
 * place to be wrong instead of one per component — and so a rejected command
 * always arrives as a `RimaiaError` with a readable `message`, never as an
 * object that stringifies to `[object Object]`.
 */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (thrown) {
    throw toRimaiaError(thrown);
  }
}

/**
 * Anything can come back across the IPC boundary — the backend's `{code, message}`
 * payload, a plugin's bare string, or a JS exception from `invoke` itself. All
 * three have to end up renderable.
 */
export function toRimaiaError(thrown: unknown): RimaiaError {
  if (isRimaiaError(thrown)) {
    return thrown;
  }
  if (thrown instanceof Error) {
    return { code: "internal", message: thrown.message };
  }
  if (typeof thrown === "string") {
    return { code: "internal", message: thrown };
  }
  return { code: "internal", message: JSON.stringify(thrown) };
}

function isRimaiaError(value: unknown): value is RimaiaError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as RimaiaError).code === "string" &&
    typeof (value as RimaiaError).message === "string"
  );
}

export function getAppInfo(): Promise<AppInfo> {
  return call<AppInfo>("get_app_info");
}

export function revealAppDataDir(): Promise<void> {
  return call<void>("reveal_app_data_dir");
}

/** Only registered in debug builds — see `commands::app::debug_provoke_error`. */
export function debugProvokeError(): Promise<void> {
  return call<void>("debug_provoke_error");
}
