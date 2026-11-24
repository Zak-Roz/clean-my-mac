import { describe, expect, it } from "vitest";
import { formatThreshold, isDevBuild } from "./monitor";

describe("isDevBuild", () => {
  it("recognizes Cargo target directories only", () => {
    expect(isDevBuild("/Users/me/clean-my-mac/target/debug/cmm")).toBe(true);
    expect(isDevBuild("/Users/me/clean-my-mac/target/release/cmm")).toBe(true);
    expect(isDevBuild("/Users/me/.cargo/bin/cmm")).toBe(false);
    expect(isDevBuild("/Users/me/target/cmm")).toBe(false);
  });
});

describe("formatThreshold", () => {
  it("keeps round gigabytes whole", () => {
    expect(formatThreshold(50e9)).toBe("50 GB");
    expect(formatThreshold(5e9)).toBe("5 GB");
    expect(formatThreshold(150e9)).toBe("150 GB");
    expect(formatThreshold(7.5e9)).toBe("7.5 GB");
    expect(formatThreshold(500e6)).toBe("500 MB");
  });
});
