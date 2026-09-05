import { useMemo } from "react";
import { FileText, Search } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  closestCenter,
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { ManagementListSearch } from "@/components/common/ManagementListSearch";
import { ScrollArea } from "@/components/ui/scroll-area";
import type { Prompt } from "@/lib/api";
import PromptListItem from "./PromptListItem";

interface PromptLibraryProps {
  prompts: Record<string, Prompt>;
  orderedIds: string[];
  enabledIds: ReadonlySet<string>;
  loading: boolean;
  searchQuery: string;
  statusText: string;
  disabled?: boolean;
  reorderEnabled?: boolean;
  onSearchQueryChange: (value: string) => void;
  onToggle: (id: string, enabled: boolean) => void;
  onReorder: (activeId: string, overId: string) => void;
  onEdit: (id: string) => void;
  onDelete: (id: string) => void;
  isDeleteDisabled?: (id: string, prompt: Prompt) => boolean;
  getDeleteTitle?: (id: string, prompt: Prompt) => string;
}

export function PromptLibrary({
  prompts,
  orderedIds,
  enabledIds,
  loading,
  searchQuery,
  statusText,
  disabled = false,
  reorderEnabled = true,
  onSearchQueryChange,
  onToggle,
  onReorder,
  onEdit,
  onDelete,
  isDeleteDisabled,
  getDeleteTitle,
}: PromptLibraryProps) {
  const { t } = useTranslation();
  const promptCount = Object.keys(prompts).length;
  const normalizedSearchQuery = searchQuery.trim().toLocaleLowerCase();
  const filteredIds = useMemo(() => {
    if (!normalizedSearchQuery) return orderedIds;

    return orderedIds.filter((id) => {
      const prompt = prompts[id];
      if (!prompt) return false;
      return [
        id,
        prompt.id,
        prompt.name,
        prompt.description,
        prompt.content,
      ].some((value) =>
        value?.toLocaleLowerCase().includes(normalizedSearchQuery),
      );
    });
  }, [normalizedSearchQuery, orderedIds, prompts]);

  const canReorder = reorderEnabled && !normalizedSearchQuery && !disabled;
  const sensors = useSensors(
    useSensor(PointerSensor, {
      activationConstraint: { distance: 8 },
    }),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
    }),
  );

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    onReorder(String(active.id), String(over.id));
  };

  return (
    <>
      <div className="mb-4 flex-shrink-0 rounded-xl border border-white/10 px-6 py-4 glass">
        <div className="text-sm text-muted-foreground">{statusText}</div>
      </div>

      <ManagementListSearch
        value={searchQuery}
        onValueChange={onSearchQueryChange}
        placeholder={t("prompts.searchPlaceholder")}
        ariaLabel={t("prompts.searchAriaLabel")}
        clearLabel={t("common.clear")}
      />

      <ScrollArea className="-mr-3 min-h-0 flex-1" type="auto">
        <div className="pb-16 pr-3">
          {loading ? (
            <div className="py-12 text-center text-muted-foreground">
              {t("prompts.loading")}
            </div>
          ) : promptCount === 0 ? (
            <div className="py-12 text-center">
              <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-muted">
                <FileText size={24} className="text-muted-foreground" />
              </div>
              <h3 className="mb-2 text-lg font-medium text-foreground">
                {t("prompts.empty")}
              </h3>
              <p className="text-sm text-muted-foreground">
                {t("prompts.emptyDescription")}
              </p>
            </div>
          ) : filteredIds.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-center text-muted-foreground">
              <Search className="mb-4 h-10 w-10 opacity-40" />
              <p className="text-sm">{t("prompts.noSearchResults")}</p>
            </div>
          ) : (
            <DndContext
              sensors={sensors}
              collisionDetection={closestCenter}
              onDragEnd={handleDragEnd}
            >
              <SortableContext
                items={filteredIds}
                strategy={verticalListSortingStrategy}
              >
                <div className="space-y-3">
                  {filteredIds.map((id) => {
                    const prompt = prompts[id];
                    if (!prompt) return null;
                    return (
                      <PromptListItem
                        key={id}
                        id={id}
                        prompt={prompt}
                        enabled={enabledIds.has(id)}
                        onToggle={onToggle}
                        onEdit={onEdit}
                        onDelete={onDelete}
                        disabled={disabled}
                        reorderEnabled={canReorder}
                        deleteDisabled={isDeleteDisabled?.(id, prompt)}
                        deleteTitle={getDeleteTitle?.(id, prompt)}
                      />
                    );
                  })}
                </div>
              </SortableContext>
            </DndContext>
          )}
        </div>
      </ScrollArea>
    </>
  );
}
