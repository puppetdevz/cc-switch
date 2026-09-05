import { ChevronDown } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";

interface PromptComposePreviewProps {
  content: string;
  emptyHint: string;
  dirty: boolean;
  applying?: boolean;
  onApply: () => void;
  onDiscard: () => void;
}

export function PromptComposePreview({
  content,
  emptyHint,
  dirty,
  applying = false,
  onApply,
  onDiscard,
}: PromptComposePreviewProps) {
  const { t } = useTranslation();

  return (
    <div className="mb-4 flex-shrink-0 space-y-3">
      <Collapsible defaultOpen={false}>
        <CollapsibleTrigger className="flex w-full items-center justify-between rounded-xl border border-border-default bg-muted/40 px-4 py-3 text-left text-sm hover:bg-muted/60">
          <span className="font-medium text-foreground">
            {t("prompts.preview")}
            {dirty ? (
              <span className="ml-2 text-xs font-normal text-amber-600 dark:text-amber-400">
                {t("prompts.draftUnapplied")}
              </span>
            ) : null}
          </span>
          <ChevronDown className="h-4 w-4 text-muted-foreground transition-transform [[data-state=open]_&]:rotate-180" />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-xl border border-white/10 bg-muted/40 p-4 font-mono text-xs text-foreground">
            {content || emptyHint}
          </pre>
        </CollapsibleContent>
      </Collapsible>

      {dirty ? (
        <div className="flex justify-end gap-2">
          <Button
            type="button"
            variant="outline"
            disabled={applying}
            onClick={onDiscard}
          >
            {t("prompts.discard")}
          </Button>
          <Button type="button" disabled={applying} onClick={onApply}>
            {t("prompts.apply")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
