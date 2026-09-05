import React, { type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { CSS } from "@dnd-kit/utilities";
import { useSortable } from "@dnd-kit/sortable";
import { Edit3, GripVertical, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { Prompt } from "@/lib/api";
import PromptToggle from "./PromptToggle";

interface PromptListItemProps {
  id: string;
  prompt: Prompt;
  enabled: boolean;
  onToggle: (id: string, enabled: boolean) => void;
  onEdit: (id: string) => void;
  onDelete: (id: string) => void;
  disabled?: boolean;
  deleteDisabled?: boolean;
  deleteTitle?: string;
  reorderEnabled?: boolean;
}

const PromptListItem: React.FC<PromptListItemProps> = ({
  id,
  prompt,
  enabled,
  onToggle,
  onEdit,
  onDelete,
  disabled = false,
  deleteDisabled = false,
  deleteTitle,
  reorderEnabled = false,
}) => {
  const { t } = useTranslation();
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id, disabled: !reorderEnabled || disabled });

  const style: CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
  };

  return (
    <div
      ref={setNodeRef}
      style={style}
      className={cn(
        "group relative h-16 rounded-xl border border-border-default bg-muted/50 p-4 transition-all duration-300 hover:bg-muted hover:border-border-default/80 hover:shadow-sm",
        isDragging && "z-10 cursor-grabbing border-primary shadow-lg",
      )}
    >
      <div className="flex items-center gap-3 h-full">
        {reorderEnabled ? (
          <button
            type="button"
            className={cn(
              "-ml-1.5 flex-shrink-0 cursor-grab p-1.5 text-muted-foreground/50 transition-colors hover:text-muted-foreground active:cursor-grabbing",
              disabled && "cursor-not-allowed opacity-50",
              isDragging && "cursor-grabbing",
            )}
            aria-label={t("prompts.dragHandle")}
            disabled={disabled}
            {...attributes}
            {...listeners}
          >
            <GripVertical className="h-4 w-4" />
          </button>
        ) : null}

        <div className="flex-shrink-0">
          <PromptToggle
            enabled={enabled}
            onChange={(newEnabled) => onToggle(id, newEnabled)}
            disabled={disabled}
          />
        </div>

        <div className="flex-1 min-w-0">
          <h3 className="font-medium text-foreground mb-1">{prompt.name}</h3>
          {prompt.description && (
            <p className="text-sm text-muted-foreground truncate">
              {prompt.description}
            </p>
          )}
        </div>

        <div className="flex items-center gap-2 flex-shrink-0">
          <Button
            type="button"
            variant="ghost"
            size="icon"
            onClick={() => onEdit(id)}
            disabled={disabled}
            className="disabled:opacity-100"
            title={t("common.edit")}
          >
            <Edit3 size={16} />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            onClick={() => onDelete(id)}
            disabled={disabled || deleteDisabled}
            className="hover:text-red-500 hover:bg-red-100 disabled:opacity-100 dark:hover:text-red-400 dark:hover:bg-red-500/10"
            title={deleteTitle ?? t("common.delete")}
          >
            <Trash2 size={16} />
          </Button>
        </div>
      </div>
    </div>
  );
};

export default PromptListItem;
