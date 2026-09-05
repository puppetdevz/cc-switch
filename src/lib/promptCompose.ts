import type { Prompt } from "@/lib/api";

export interface PromptDraft {
  order: string[];
  enabledIds: string[];
}

export function sortPromptEntries(
  prompts: Record<string, Prompt>,
): [string, Prompt][] {
  return Object.entries(prompts).sort(([, left], [, right]) => {
    const leftOrder = left.sortOrder;
    const rightOrder = right.sortOrder;
    if (leftOrder != null && rightOrder != null && leftOrder !== rightOrder) {
      return leftOrder - rightOrder;
    }
    if (leftOrder != null && rightOrder == null) return -1;
    if (leftOrder == null && rightOrder != null) return 1;

    const leftCreated = left.createdAt ?? 0;
    const rightCreated = right.createdAt ?? 0;
    if (leftCreated !== rightCreated) {
      return leftCreated - rightCreated;
    }
    return left.id.localeCompare(right.id);
  });
}

export function snapshotPromptDraft(
  prompts: Record<string, Prompt>,
): PromptDraft {
  const order = sortPromptEntries(prompts).map(([id]) => id);
  return {
    order,
    enabledIds: order.filter((id) => prompts[id]?.enabled === true),
  };
}

export function draftsEqual(left: PromptDraft, right: PromptDraft): boolean {
  return (
    left.order.length === right.order.length &&
    left.order.every((id, index) => id === right.order[index]) &&
    left.enabledIds.length === right.enabledIds.length &&
    left.enabledIds.every((id, index) => id === right.enabledIds[index])
  );
}

export function composePromptBlocks(
  prompts: Record<string, Prompt>,
  order: string[],
  enabledIds: readonly string[],
): string {
  const enabled = new Set(enabledIds);
  return order
    .filter((id) => enabled.has(id))
    .map((id) => prompts[id]?.content ?? "")
    .join("\n\n");
}

export function visiblePromptOrder(
  prompts: Record<string, Prompt>,
  draft: PromptDraft,
  applied: PromptDraft,
): string[] {
  const known = new Set(draft.order);
  const extras = applied.order.filter(
    (id) => !known.has(id) && prompts[id] != null,
  );
  return [...draft.order.filter((id) => prompts[id] != null), ...extras];
}
