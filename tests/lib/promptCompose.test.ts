import { describe, expect, it } from "vitest";
import type { Prompt } from "@/lib/api";
import {
  composePromptBlocks,
  snapshotPromptDraft,
  visiblePromptOrder,
} from "@/lib/promptCompose";

function prompt(
  id: string,
  content: string,
  enabled: boolean,
  sortOrder?: number,
): Prompt {
  return { id, name: id, content, enabled, sortOrder };
}

describe("promptCompose", () => {
  it("snapshots prompts by sortOrder and lists enabled ids in that order", () => {
    const prompts = {
      b: prompt("b", "B", true, 1),
      a: prompt("a", "A", false, 0),
      c: prompt("c", "C", true, 2),
    };
    expect(snapshotPromptDraft(prompts)).toEqual({
      order: ["a", "b", "c"],
      enabledIds: ["b", "c"],
    });
  });

  it("composes enabled blocks with a blank line and no headings", () => {
    const prompts = {
      a: prompt("a", "alpha", true, 0),
      b: prompt("b", "beta", true, 1),
    };
    expect(composePromptBlocks(prompts, ["a", "b"], ["a", "b"])).toBe(
      "alpha\n\nbeta",
    );
    expect(composePromptBlocks(prompts, ["b", "a"], ["a", "b"])).toBe(
      "beta\n\nalpha",
    );
    expect(composePromptBlocks(prompts, ["a", "b"], ["b"])).toBe("beta");
  });

  it("appends newly applied ids that are missing from a dirty draft", () => {
    const prompts = {
      a: prompt("a", "A", true, 0),
      extra: prompt("extra", "X", false, 1),
    };
    expect(
      visiblePromptOrder(
        prompts,
        { order: ["a"], enabledIds: ["a"] },
        { order: ["a", "extra"], enabledIds: ["a"] },
      ),
    ).toEqual(["a", "extra"]);
  });
});
