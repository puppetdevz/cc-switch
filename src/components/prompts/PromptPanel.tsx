import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { type AppId } from "@/lib/api";
import { usePromptActions } from "@/hooks/usePromptActions";
import { usePromptDraft } from "@/hooks/usePromptDraft";
import { useTauriEvent } from "@/hooks/useTauriEvent";
import { composePromptBlocks } from "@/lib/promptCompose";
import PiPromptPanel, { type PromptPrimaryAction } from "./PiPromptPanel";
import PromptFormPanel from "./PromptFormPanel";
import { PromptComposePreview } from "./PromptComposePreview";
import { PromptLibrary } from "./PromptLibrary";
import { ConfirmDialog } from "../ConfirmDialog";

interface PromptPanelProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  appId: AppId;
  onInteractionBlockedChange?: (blocked: boolean) => void;
  onNavigationBlockedChange?: (blocked: boolean) => void;
  onPrimaryActionChange?: (action: PromptPrimaryAction) => void;
}

export interface PromptPanelHandle {
  openAdd: () => void;
}

export type { PromptPrimaryAction } from "./PiPromptPanel";

const StandardPromptPanel = React.forwardRef<
  PromptPanelHandle,
  PromptPanelProps
>(
  (
    {
      open,
      appId,
      onInteractionBlockedChange,
      onNavigationBlockedChange,
      onPrimaryActionChange,
    },
    ref,
  ) => {
    const { t } = useTranslation();
    const [isFormOpen, setIsFormOpen] = useState(false);
    const [editingId, setEditingId] = useState<string | null>(null);
    const [searchQuery, setSearchQuery] = useState("");
    const [confirmDialog, setConfirmDialog] = useState<{
      isOpen: boolean;
      titleKey: string;
      messageKey: string;
      messageParams?: Record<string, unknown>;
      onConfirm: () => void;
    } | null>(null);
    const [writePending, setWritePending] = useState(false);
    const [reloadPending, setReloadPending] = useState(false);
    const writeLockRef = React.useRef(false);
    const reloadLockRef = React.useRef(false);
    const reloadRunGenerationRef = React.useRef(0);
    const overlayOpenRef = React.useRef(false);
    const externalReloadQueuedRef = React.useRef(false);

    const { prompts, loading, reload, savePrompt, deletePrompt, applyPrompts } =
      usePromptActions(appId);
    const reloadRef = React.useRef(reload);
    reloadRef.current = reload;
    const {
      draft,
      applied,
      displayOrder,
      enabledSet,
      dirty,
      toggle,
      reorder,
      reset,
      markClean,
    } = usePromptDraft(prompts, appId);
    const dirtyRef = React.useRef(false);
    dirtyRef.current = dirty;

    const dialogOpen = confirmDialog !== null;
    const interactionBlocked =
      loading || reloadPending || writePending || isFormOpen || dialogOpen;
    const navigationBlocked = writePending || isFormOpen || dialogOpen || dirty;

    useEffect(() => {
      onInteractionBlockedChange?.(interactionBlocked);
    }, [interactionBlocked, onInteractionBlockedChange]);

    useEffect(() => {
      onNavigationBlockedChange?.(navigationBlocked);
    }, [navigationBlocked, onNavigationBlockedChange]);

    useEffect(() => {
      onPrimaryActionChange?.("prompt");
    }, [onPrimaryActionChange]);

    useEffect(
      () => () => {
        onInteractionBlockedChange?.(false);
        onNavigationBlockedChange?.(false);
      },
      [onInteractionBlockedChange, onNavigationBlockedChange],
    );

    const runExternalReload = React.useCallback(async () => {
      if (writeLockRef.current || overlayOpenRef.current || dirtyRef.current) {
        externalReloadQueuedRef.current = true;
        return;
      }

      const runGeneration = ++reloadRunGenerationRef.current;
      externalReloadQueuedRef.current = false;
      reloadLockRef.current = true;
      setReloadPending(true);
      try {
        await reloadRef.current();
      } finally {
        if (reloadRunGenerationRef.current === runGeneration) {
          reloadLockRef.current = false;
          setReloadPending(false);
        }
      }
    }, []);

    const beginWrite = () => {
      if (loading || reloadLockRef.current || writeLockRef.current)
        return false;
      writeLockRef.current = true;
      setWritePending(true);
      return true;
    };

    const endWrite = () => {
      writeLockRef.current = false;
      setWritePending(false);
      if (externalReloadQueuedRef.current) {
        void runExternalReload();
      }
    };

    useEffect(() => {
      if (open) void runExternalReload();
    }, [appId, open, runExternalReload]);

    useEffect(() => {
      setSearchQuery("");
      overlayOpenRef.current = false;
      setIsFormOpen(false);
      setEditingId(null);
      setConfirmDialog(null);
      if (externalReloadQueuedRef.current) {
        void runExternalReload();
      }
    }, [appId, runExternalReload]);

    useEffect(() => {
      const handlePromptImported = (event: Event) => {
        const customEvent = event as CustomEvent;
        if (customEvent.detail?.app === appId) {
          void runExternalReload();
        }
      };

      window.addEventListener("prompt-imported", handlePromptImported);
      return () => {
        window.removeEventListener("prompt-imported", handlePromptImported);
      };
    }, [appId, runExternalReload]);

    useTauriEvent("profile-applied", runExternalReload);

    const handleAdd = () => {
      if (reloadLockRef.current || writeLockRef.current || interactionBlocked) {
        return;
      }
      overlayOpenRef.current = true;
      setEditingId(null);
      setIsFormOpen(true);
    };

    React.useImperativeHandle(ref, () => ({
      openAdd: handleAdd,
    }));

    const handleEdit = (id: string) => {
      if (reloadLockRef.current || writeLockRef.current || interactionBlocked) {
        return;
      }
      overlayOpenRef.current = true;
      setEditingId(id);
      setIsFormOpen(true);
    };

    const handleDelete = (id: string) => {
      if (reloadLockRef.current || writeLockRef.current || interactionBlocked) {
        return;
      }
      const prompt = prompts[id];
      overlayOpenRef.current = true;
      setConfirmDialog({
        isOpen: true,
        titleKey: "prompts.confirm.deleteTitle",
        messageKey: "prompts.confirm.deleteMessage",
        messageParams: { name: prompt?.name },
        onConfirm: async () => {
          if (!beginWrite()) return;
          try {
            const refreshed = await deletePrompt(id);
            if (refreshed === false) {
              externalReloadQueuedRef.current = true;
            }
            overlayOpenRef.current = false;
            setConfirmDialog(null);
          } catch {
            // Error handled by hook
          } finally {
            endWrite();
          }
        },
      });
    };

    const handleToggle = (id: string, enabled: boolean) => {
      if (interactionBlocked) return;
      toggle(id, enabled);
    };

    const handleApply = async () => {
      if (!beginWrite()) return;
      try {
        const refreshed = await applyPrompts(draft.order, draft.enabledIds);
        markClean();
        dirtyRef.current = false;
        if (refreshed === false) {
          externalReloadQueuedRef.current = true;
        }
      } catch {
        // Error handled by hook
      } finally {
        endWrite();
      }
    };

    const handleSave = async (
      id: string,
      prompt: Parameters<typeof savePrompt>[1],
    ) => {
      if (!beginWrite()) return false;
      try {
        const refreshed = await savePrompt(id, prompt);
        if (refreshed === false) {
          externalReloadQueuedRef.current = true;
        }
        return true;
      } catch {
        // Error handled by hook
        return false;
      } finally {
        endWrite();
      }
    };

    const handleCloseForm = () => {
      if (writeLockRef.current) return;
      overlayOpenRef.current = false;
      setIsFormOpen(false);
      setEditingId(null);
      if (externalReloadQueuedRef.current) {
        void runExternalReload();
      }
    };

    const appliedCount = applied.enabledIds.length;
    const appliedNames = applied.enabledIds
      .map((id) => prompts[id]?.name)
      .filter((name): name is string => Boolean(name));
    const statusText = useMemo(() => {
      const parts = [
        t("prompts.count", { count: Object.keys(prompts).length }),
      ];
      if (appliedCount === 1 && appliedNames[0]) {
        parts.push(t("prompts.enabledName", { name: appliedNames[0] }));
      } else if (appliedCount > 1) {
        parts.push(t("prompts.appliedCount", { count: appliedCount }));
      } else {
        parts.push(t("prompts.noneEnabled"));
      }
      if (dirty) {
        parts.push(t("prompts.draftUnapplied"));
      }
      return parts.join(" · ");
    }, [appliedCount, appliedNames, dirty, prompts, t]);
    const previewContent = composePromptBlocks(
      prompts,
      displayOrder,
      draft.enabledIds,
    );

    return (
      <div className="flex flex-col flex-1 min-h-0 px-6">
        <PromptComposePreview
          content={previewContent}
          emptyHint={t("prompts.previewEmpty")}
          dirty={dirty}
          applying={writePending}
          onApply={() => {
            void handleApply();
          }}
          onDiscard={() => {
            dirtyRef.current = false;
            reset();
            if (externalReloadQueuedRef.current) {
              void runExternalReload();
            }
          }}
        />
        <PromptLibrary
          prompts={prompts}
          orderedIds={displayOrder}
          enabledIds={enabledSet}
          loading={loading}
          searchQuery={searchQuery}
          statusText={statusText}
          disabled={interactionBlocked}
          onSearchQueryChange={setSearchQuery}
          onToggle={handleToggle}
          onReorder={reorder}
          onEdit={handleEdit}
          onDelete={handleDelete}
          isDeleteDisabled={(_id, prompt) => prompt.enabled}
          getDeleteTitle={(_id, prompt) =>
            prompt.enabled ? t("prompts.stopBeforeDelete") : t("common.delete")
          }
        />

        {isFormOpen && (
          <PromptFormPanel
            appId={appId}
            editingId={editingId || undefined}
            initialData={editingId ? prompts[editingId] : undefined}
            onSave={handleSave}
            onClose={handleCloseForm}
          />
        )}

        {confirmDialog && (
          <ConfirmDialog
            isOpen={confirmDialog.isOpen}
            title={t(confirmDialog.titleKey)}
            message={t(confirmDialog.messageKey, confirmDialog.messageParams)}
            pending={writePending}
            onConfirm={confirmDialog.onConfirm}
            onCancel={() => {
              if (!writeLockRef.current) {
                overlayOpenRef.current = false;
                setConfirmDialog(null);
                if (externalReloadQueuedRef.current) {
                  void runExternalReload();
                }
              }
            }}
          />
        )}
      </div>
    );
  },
);

StandardPromptPanel.displayName = "StandardPromptPanel";

const PromptPanel = React.forwardRef<PromptPanelHandle, PromptPanelProps>(
  (props, ref) => {
    if (props.appId === "pi") {
      return (
        <PiPromptPanel
          ref={ref}
          open={props.open}
          onInteractionBlockedChange={props.onInteractionBlockedChange}
          onNavigationBlockedChange={props.onNavigationBlockedChange}
          onPrimaryActionChange={props.onPrimaryActionChange}
        />
      );
    }

    return <StandardPromptPanel ref={ref} {...props} />;
  },
);

PromptPanel.displayName = "PromptPanel";

export default PromptPanel;
