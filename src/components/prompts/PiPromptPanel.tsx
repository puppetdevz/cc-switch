import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ScrollArea } from "@/components/ui/scroll-area";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { usePromptActions } from "@/hooks/usePromptActions";
import { usePromptDraft } from "@/hooks/usePromptDraft";
import { useTauriEvent } from "@/hooks/useTauriEvent";
import { composePromptBlocks } from "@/lib/promptCompose";
import type { Prompt } from "@/lib/api";
import PromptFormPanel from "./PromptFormPanel";
import { PromptComposePreview } from "./PromptComposePreview";
import { PromptLibrary } from "./PromptLibrary";
import {
  PiPromptTemplates,
  PiSystemPromptFiles,
  type PiPromptTemplatesHandle,
} from "./PiNativePromptResources";

export type PiPromptTab = "global" | "system" | "templates";
export type PromptPrimaryAction = "prompt" | "template" | null;

interface PiPromptPanelProps {
  open: boolean;
  onInteractionBlockedChange?: (blocked: boolean) => void;
  onNavigationBlockedChange?: (blocked: boolean) => void;
  onPrimaryActionChange?: (action: PromptPrimaryAction) => void;
}

export interface PiPromptPanelHandle {
  openAdd: () => void;
}

const actionForTab = (tab: PiPromptTab): PromptPrimaryAction => {
  if (tab === "global") return "prompt";
  if (tab === "templates") return "template";
  return null;
};

const PiPromptPanel = React.forwardRef<PiPromptPanelHandle, PiPromptPanelProps>(
  (
    {
      open,
      onInteractionBlockedChange,
      onNavigationBlockedChange,
      onPrimaryActionChange,
    },
    ref,
  ) => {
    const { t } = useTranslation();
    const [activeTab, setActiveTab] = useState<PiPromptTab>("global");
    const [isFormOpen, setIsFormOpen] = useState(false);
    const [editingId, setEditingId] = useState<string | null>(null);
    const [searchQuery, setSearchQuery] = useState("");
    const [deletingPrompt, setDeletingPrompt] = useState<Prompt | null>(null);
    const templatesRef = useRef<PiPromptTemplatesHandle>(null);

    const {
      prompts,
      loading,
      currentFileContent,
      reload,
      savePrompt,
      deletePrompt,
      applyPrompts,
    } = usePromptActions("pi");
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
    } = usePromptDraft(prompts, "pi");
    const dirtyRef = useRef(false);
    dirtyRef.current = dirty;
    const queuedReloadRef = useRef(false);
    const [applying, setApplying] = useState(false);
    const dialogOpen = deletingPrompt !== null;
    const writePending = applying;
    const interactionBlocked =
      loading || writePending || isFormOpen || dialogOpen;
    const navigationBlocked = writePending || isFormOpen || dialogOpen || dirty;

    useEffect(() => {
      if (open) void reload();
    }, [open, reload]);

    useEffect(() => {
      onPrimaryActionChange?.(actionForTab(activeTab));
    }, [activeTab, onPrimaryActionChange]);

    useEffect(() => {
      onInteractionBlockedChange?.(interactionBlocked);
    }, [interactionBlocked, onInteractionBlockedChange]);

    useEffect(() => {
      onNavigationBlockedChange?.(navigationBlocked);
    }, [navigationBlocked, onNavigationBlockedChange]);

    useEffect(
      () => () => {
        onInteractionBlockedChange?.(false);
        onNavigationBlockedChange?.(false);
      },
      [onInteractionBlockedChange, onNavigationBlockedChange],
    );

    const runReload = React.useCallback(() => {
      if (dirtyRef.current || writePending || isFormOpen || dialogOpen) {
        queuedReloadRef.current = true;
        return;
      }
      queuedReloadRef.current = false;
      void reload();
    }, [dialogOpen, isFormOpen, reload, writePending]);

    useEffect(() => {
      const handlePromptImported = (event: Event) => {
        const customEvent = event as CustomEvent;
        if (customEvent.detail?.app === "pi") {
          runReload();
        }
      };

      window.addEventListener("prompt-imported", handlePromptImported);
      return () =>
        window.removeEventListener("prompt-imported", handlePromptImported);
    }, [runReload]);

    useTauriEvent("profile-applied", runReload);

    const openGlobalPromptForm = (id?: string) => {
      setEditingId(id ?? null);
      setIsFormOpen(true);
    };

    React.useImperativeHandle(
      ref,
      () => ({
        openAdd: () => {
          if (activeTab === "global") {
            openGlobalPromptForm();
          } else if (activeTab === "templates") {
            templatesRef.current?.openCreate();
          }
        },
      }),
      [activeTab],
    );

    const appliedCount = applied.enabledIds.length;
    const appliedNames = applied.enabledIds
      .map((id) => prompts[id]?.name)
      .filter((name): name is string => Boolean(name));
    const hasExternalPrompt =
      Boolean(currentFileContent?.trim()) && appliedCount === 0;
    const statusText = useMemo(() => {
      const parts = [
        t("prompts.count", { count: Object.keys(prompts).length }),
      ];
      if (appliedCount === 1 && appliedNames[0]) {
        parts.push(t("prompts.enabledName", { name: appliedNames[0] }));
      } else if (appliedCount > 1) {
        parts.push(t("prompts.appliedCount", { count: appliedCount }));
      } else if (hasExternalPrompt) {
        parts.push(t("pi.prompts.externalAgents"));
      } else {
        parts.push(t("prompts.noneEnabled"));
      }
      if (dirty) {
        parts.push(t("prompts.draftUnapplied"));
      }
      return parts.join(" · ");
    }, [appliedCount, appliedNames, dirty, hasExternalPrompt, prompts, t]);
    const previewContent = composePromptBlocks(
      prompts,
      displayOrder,
      draft.enabledIds,
    );

    const handleApply = async () => {
      if (interactionBlocked) return;
      setApplying(true);
      try {
        await applyPrompts(draft.order, draft.enabledIds);
        markClean();
        dirtyRef.current = false;
      } catch {
        // usePromptActions owns the error toast.
      } finally {
        setApplying(false);
        if (queuedReloadRef.current) {
          queuedReloadRef.current = false;
          void reload();
        }
      }
    };

    const handleDelete = async () => {
      if (!deletingPrompt) return;
      try {
        await deletePrompt(deletingPrompt.id);
        setDeletingPrompt(null);
      } catch {
        // usePromptActions owns the error toast.
      }
    };

    return (
      <div className="flex min-h-0 flex-1 flex-col px-6">
        <Tabs
          value={activeTab}
          onValueChange={(value) => setActiveTab(value as PiPromptTab)}
          className="flex min-h-0 flex-1 flex-col"
        >
          <div className="flex shrink-0 py-4">
            <TabsList className="self-start">
              <TabsTrigger value="global">
                {t("pi.prompts.globalTab")}
              </TabsTrigger>
              <TabsTrigger value="system">
                {t("pi.prompts.systemTab")}
              </TabsTrigger>
              <TabsTrigger value="templates">
                {t("pi.prompts.templatesTab")}
              </TabsTrigger>
            </TabsList>
          </div>

          <TabsContent
            value="global"
            className="m-0 min-h-0 flex-1 data-[state=active]:flex data-[state=active]:flex-col"
          >
            <PromptComposePreview
              content={previewContent}
              emptyHint={t("prompts.previewEmptyPi")}
              dirty={dirty}
              applying={applying}
              onApply={() => {
                void handleApply();
              }}
              onDiscard={() => {
                dirtyRef.current = false;
                reset();
                if (queuedReloadRef.current) {
                  queuedReloadRef.current = false;
                  void reload();
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
              onToggle={(id, enabled) => {
                if (!interactionBlocked) toggle(id, enabled);
              }}
              onReorder={reorder}
              onEdit={openGlobalPromptForm}
              onDelete={(id) => {
                const prompt = prompts[id];
                if (prompt) setDeletingPrompt(prompt);
              }}
              isDeleteDisabled={(_id, prompt) => prompt.enabled}
              getDeleteTitle={(_id, prompt) =>
                prompt.enabled
                  ? t("prompts.stopBeforeDelete")
                  : t("common.delete")
              }
            />
          </TabsContent>

          <TabsContent
            value="system"
            className="m-0 min-h-0 flex-1 overflow-hidden"
          >
            <ScrollArea className="-mr-3 h-full" type="auto">
              <div className="pb-16 pr-3">
                <PiSystemPromptFiles />
              </div>
            </ScrollArea>
          </TabsContent>

          <TabsContent value="templates" className="m-0 min-h-0 min-w-0 flex-1">
            <PiPromptTemplates ref={templatesRef} />
          </TabsContent>
        </Tabs>

        {isFormOpen && (
          <PromptFormPanel
            appId="pi"
            editingId={editingId ?? undefined}
            initialData={editingId ? prompts[editingId] : undefined}
            onSave={savePrompt}
            onClose={() => setIsFormOpen(false)}
          />
        )}

        <ConfirmDialog
          isOpen={Boolean(deletingPrompt)}
          title={t("prompts.confirm.deleteTitle")}
          message={t("prompts.confirm.deleteMessage", {
            name: deletingPrompt?.name,
          })}
          onConfirm={() => void handleDelete()}
          onCancel={() => setDeletingPrompt(null)}
        />
      </div>
    );
  },
);

PiPromptPanel.displayName = "PiPromptPanel";

export default PiPromptPanel;
