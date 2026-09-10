import { toast } from "sonner";
import type { SyncOperationReport } from "@/types";

type Translate = (key: string, opts?: Record<string, unknown>) => string;

export function handleReport(
  report: SyncOperationReport | { status: string; warning?: string },
  t: Translate,
  options?: { successKey?: string; failureKey?: string },
) {
  if (report.status === "paused") {
    toast.info(t("settings.cloudSync.paused"));
    return;
  }
  if (report.status === "conflict") {
    toast.error(t("settings.cloudSync.conflict"));
    return;
  }
  if (report.status === "error") {
    toast.error(
      options?.failureKey
        ? t(options.failureKey, { error: report.status })
        : t("settings.cloudSync.operationFailed"),
    );
    return;
  }
  const warning =
    ("warning" in report && report.warning) ||
    ("warnings" in report &&
      report.warnings?.some(
        (item) =>
          item.code === "sync.post_operation_sync_failed" ||
          item.code === "cleanup_incomplete",
      ));
  if (warning) {
    toast.warning(t("settings.cloudSync.partialProjection"));
  }
  if (report.status === "success" || !report.status) {
    toast.success(
      options?.successKey
        ? t(options.successKey)
        : t("settings.cloudSync.operationSuccess"),
    );
    return;
  }
  toast.error(t("settings.cloudSync.operationFailed"));
}
