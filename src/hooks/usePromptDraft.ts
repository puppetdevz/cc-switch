import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { arrayMove } from "@dnd-kit/sortable";
import type { Prompt } from "@/lib/api";
import {
  draftsEqual,
  snapshotPromptDraft,
  visiblePromptOrder,
  type PromptDraft,
} from "@/lib/promptCompose";

export function usePromptDraft(
  prompts: Record<string, Prompt>,
  resetKey: string,
) {
  const applied = useMemo(() => snapshotPromptDraft(prompts), [prompts]);
  const [draft, setDraft] = useState<PromptDraft>(applied);
  const userDraftRef = useRef(false);

  useEffect(() => {
    userDraftRef.current = false;
    setDraft(snapshotPromptDraft(prompts));
    // Reset when switching apps/panels; `prompts` is read on that boundary.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resetKey]);

  useEffect(() => {
    if (!userDraftRef.current) {
      setDraft(applied);
    }
  }, [applied]);

  const dirty = userDraftRef.current && !draftsEqual(draft, applied);

  const displayOrder = useMemo(
    () => visiblePromptOrder(prompts, draft, applied),
    [applied, draft, prompts],
  );

  const enabledSet = useMemo(
    () => new Set(draft.enabledIds),
    [draft.enabledIds],
  );

  const toggle = useCallback((id: string, enabled: boolean) => {
    userDraftRef.current = true;
    setDraft((current) => {
      const selected = new Set(current.enabledIds);
      if (enabled) {
        selected.add(id);
      } else {
        selected.delete(id);
      }
      const order = current.order.includes(id)
        ? current.order
        : [...current.order, id];
      return {
        order,
        enabledIds: order.filter((item) => selected.has(item)),
      };
    });
  }, []);

  const reorder = useCallback((activeId: string, overId: string) => {
    userDraftRef.current = true;
    setDraft((current) => {
      const oldIndex = current.order.indexOf(activeId);
      const newIndex = current.order.indexOf(overId);
      if (oldIndex === -1 || newIndex === -1 || oldIndex === newIndex) {
        return current;
      }
      const order = arrayMove(current.order, oldIndex, newIndex);
      const selected = new Set(current.enabledIds);
      return {
        order,
        enabledIds: order.filter((id) => selected.has(id)),
      };
    });
  }, []);

  const markClean = useCallback(() => {
    userDraftRef.current = false;
  }, []);

  const reset = useCallback(() => {
    userDraftRef.current = false;
    setDraft(applied);
  }, [applied]);

  return {
    draft,
    applied,
    displayOrder,
    enabledSet,
    dirty,
    toggle,
    reorder,
    reset,
    markClean,
  };
}
