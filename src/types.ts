/** Mirrors `rimaia_core::ErrorCode`. Coarse on purpose: it picks a presentation,
 *  it does not reimplement backend logic. */
export type ErrorCode =
  | "database"
  | "io"
  | "not_found"
  | "invalid"
  | "internal";

/** Mirrors the payload `rimaia_core::Error` serializes to. */
export interface RimaiaError {
  code: ErrorCode;
  message: string;
}

/** Mirrors `AppInfo` in `src-tauri/src/commands/app.rs`. */
export interface AppInfo {
  appVersion: string;
  dataDir: string;
  dbFile: string;
  logsDir: string;
}

export type View = "board" | "runs" | "settings";
