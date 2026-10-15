import { describe, expect, it } from "vitest";
import { formatBytes, plural, relativeTime } from "./format";
import { errorAction, errorTitle, sentence } from "./errors";

describe("format", () => {
  it("formats sizes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(999)).toBe("999 B");
    expect(formatBytes(18_874_368)).toBe("18.9 MB");
    expect(formatBytes(734_003_200)).toBe("734 MB");
    expect(formatBytes(5 * 1024 ** 3)).toBe("5.4 GB");
  });

  it("formats relative times", () => {
    const now = new Date("2026-01-01T12:00:00Z");
    expect(relativeTime("2026-01-01T11:59:50Z", now)).toBe("just now");
    expect(relativeTime("2026-01-01T11:50:00Z", now)).toBe("10 min ago");
    expect(relativeTime("2026-01-01T09:00:00Z", now)).toBe("3 h ago");
    expect(relativeTime("2025-12-30T12:00:00Z", now)).toBe("2 d ago");
    expect(relativeTime(null, now)).toBe("");
  });

  it("pluralizes", () => {
    expect(plural(1, "drive")).toBe("1 drive");
    expect(plural(3, "drive")).toBe("3 drives");
  });
});

describe("errors", () => {
  it("maps codes to titles and actions", () => {
    expect(errorTitle({ code: "auth_required", message: "x" })).toBe("Sign-in needed");
    expect(errorAction({ code: "auth_required", message: "x" })).toBe("sign_in");
    expect(errorAction({ code: "already_mounted", message: "x" })).toBe("pick_letter");
    expect(errorAction({ code: "internal", message: "x" })).toBe("none");
    expect(sentence("access is forbidden")).toBe("Access is forbidden.");
    expect(sentence("Done!")).toBe("Done!");
  });
});
