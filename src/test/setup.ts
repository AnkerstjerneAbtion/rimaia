import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Unmount whatever the previous test rendered so DOM state and event
// listeners never leak across test cases.
afterEach(() => {
  cleanup();
});
