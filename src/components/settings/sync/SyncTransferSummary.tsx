import { useTranslation } from "react-i18next";
import { describeSyncScope, formatSyncBytes } from "@/lib/syncCategories";
import type { CloudSyncStats, RemoteSnapshotInfo } from "@/types";

export function SyncTransferSummary({
  stats,
  kind,
  remote,
}: {
  stats: CloudSyncStats | null;
  kind: "upload" | "download";
  remote?: RemoteSnapshotInfo | null;
}) {
  const { t } = useTranslation();
  const scope = describeSyncScope(stats, kind);
  const legacy = scope.legacyCombined || Boolean(remote?.legacyCombined);

  return (
    <div className="space-y-2 text-xs" data-testid={`sync-transfer-summary-${kind}`}>
      <p className="font-medium text-foreground">
        {t("settings.cloudSync.participating")}
      </p>
      <ul className="list-disc space-y-1 pl-5 text-muted-foreground">
        {scope.participating.length === 0 ? (
          <li>{t("settings.cloudSync.paused")}</li>
        ) : (
          scope.participating.map((row) => (
            <li key={row.category}>
              {t(`settings.cloudSync.categories.${row.category}.name`)}
              {" · "}
              {formatSyncBytes(
                kind === "download" ? row.remoteBytes : row.localBytes,
              )}
            </li>
          ))
        )}
      </ul>
      {scope.skipped.length > 0 && (
        <>
          <p className="font-medium text-foreground">
            {t("settings.cloudSync.skipped")}
          </p>
          <ul className="list-disc space-y-1 pl-5 text-muted-foreground">
            {scope.skipped.map((row) => (
              <li key={row.category}>
                {t(`settings.cloudSync.categories.${row.category}.name`)}
                {" · "}
                {t(
                  `settings.cloudSync.status.${row.enabled ? row.status : "disabled"}`,
                )}
              </li>
            ))}
          </ul>
        </>
      )}
      {kind === "download" && scope.participating.length > 0 && (
        <p className="text-destructive">
          {t("settings.cloudSync.willReplace")}
          {": "}
          {scope.participating
            .map((row) =>
              t(`settings.cloudSync.categories.${row.category}.name`),
            )
            .join(", ")}
        </p>
      )}
      <p className="text-muted-foreground">
        {t("settings.cloudSync.estimatedBytes", {
          size: formatSyncBytes(scope.estimated),
        })}
      </p>
      {scope.sensitive && (
        <p className="text-amber-600 dark:text-amber-400">
          {t("settings.cloudSync.sensitiveHint")}
        </p>
      )}
      {legacy && (
        <p className="text-amber-600 dark:text-amber-400">
          {t("settings.cloudSync.legacyCombined")}
        </p>
      )}
    </div>
  );
}
