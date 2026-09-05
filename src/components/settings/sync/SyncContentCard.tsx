import { useCallback, useEffect, useMemo, useState } from "react";
import { Loader2, ShieldAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { settingsApi } from "@/lib/api";
import { applyCategoryToggle, formatSyncBytes } from "@/lib/syncCategories";
import { handleReport } from "@/lib/syncReports";
import type {
  CategoryUiState,
  CloudRemoteInventory,
  CloudSyncSelection,
  CloudSyncStats,
  SyncCategory,
} from "@/types";

const CATEGORY_ORDER: SyncCategory[] = [
  "providers",
  "mcp",
  "prompts",
  "skill_repos",
  "skill_metadata",
  "skill_files",
  "profiles",
  "common_config",
  "proxy_settings",
  "diagnostics_settings",
  "model_pricing",
];

function selectionToRecord(
  selection: CloudSyncSelection,
): Record<SyncCategory, boolean> {
  return {
    providers: selection.providers,
    mcp: selection.mcp,
    prompts: selection.prompts,
    skill_repos: selection.skillRepos,
    skill_metadata: selection.skillMetadata,
    skill_files: selection.skillFiles,
    profiles: selection.profiles,
    common_config: selection.commonConfig,
    proxy_settings: selection.proxySettings,
    diagnostics_settings: selection.diagnosticsSettings,
    model_pricing: selection.modelPricing,
  };
}

function recordToSelection(
  record: Record<SyncCategory, boolean>,
): CloudSyncSelection {
  return {
    providers: record.providers,
    mcp: record.mcp,
    prompts: record.prompts,
    skillRepos: record.skill_repos,
    skillMetadata: record.skill_metadata,
    skillFiles: record.skill_files,
    profiles: record.profiles,
    commonConfig: record.common_config,
    proxySettings: record.proxy_settings,
    diagnosticsSettings: record.diagnostics_settings,
    modelPricing: record.model_pricing,
  };
}

export function SyncContentCard({
  configured,
  pausedUploadDownload,
  onStats,
}: {
  configured: boolean;
  pausedUploadDownload?: (paused: boolean) => void;
  onStats?: (stats: CloudSyncStats | null) => void;
}) {
  const { t } = useTranslation();
  const [stats, setStats] = useState<CloudSyncStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [pendingCategory, setPendingCategory] = useState<CategoryUiState | null>(
    null,
  );
  const [manageOpen, setManageOpen] = useState(false);
  const [inventory, setInventory] = useState<CloudRemoteInventory | null>(null);
  const [deleteTargets, setDeleteTargets] = useState<SyncCategory[]>([]);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [confirmV2, setConfirmV2] = useState(false);

  const loadStats = useCallback(async () => {
    setLoading(true);
    try {
      const next = await settingsApi.cloudSyncGetCategoryStats();
      setStats(next);
      pausedUploadDownload?.(next.paused);
      onStats?.(next);
    } catch {
      setStats(null);
      onStats?.(null);
    } finally {
      setLoading(false);
    }
  }, [pausedUploadDownload, onStats]);

  useEffect(() => {
    void loadStats();
  }, [loadStats]);

  const selectionRecord = useMemo(
    () =>
      stats
        ? selectionToRecord(stats.selection)
        : (Object.fromEntries(
            CATEGORY_ORDER.map((id) => [id, true]),
          ) as Record<SyncCategory, boolean>),
    [stats],
  );

  const handleToggle = async (category: SyncCategory, enabled: boolean) => {
    const next = recordToSelection(
      applyCategoryToggle(selectionRecord, category, enabled),
    );
    setSaving(true);
    try {
      await settingsApi.cloudSyncSetSelection(next);
      await loadStats();
    } catch (error) {
      toast.error((error as Error)?.message ?? String(error));
    } finally {
      setSaving(false);
    }
  };

  const handleInit = async (direction: "upload" | "download") => {
    if (!pendingCategory) return;
    try {
      const report = await settingsApi.cloudSyncInitCategory(
        pendingCategory.category,
        direction,
        stats?.snapshotId ?? undefined,
      );
      handleReport(report, t);
      setPendingCategory(null);
      await loadStats();
    } catch (error) {
      toast.error((error as Error)?.message ?? String(error));
    }
  };

  const openManager = async () => {
    setManageOpen(true);
    try {
      const next = await settingsApi.cloudSyncRemoteInventory();
      setInventory(next);
    } catch (error) {
      toast.error((error as Error)?.message ?? String(error));
    }
  };

  const handleDelete = async () => {
    try {
      const report = await settingsApi.cloudSyncDeleteCategories(deleteTargets);
      handleReport(report, t);
      setConfirmDelete(false);
      setDeleteTargets([]);
      await openManager();
      await loadStats();
    } catch (error) {
      toast.error((error as Error)?.message ?? String(error));
    }
  };

  const handleDeleteV2 = async () => {
    try {
      const report = await settingsApi.cloudSyncDeleteV2Snapshot();
      handleReport(report, t);
      setConfirmV2(false);
      await openManager();
    } catch (error) {
      toast.error((error as Error)?.message ?? String(error));
    }
  };

  return (
    <div className="space-y-4 rounded-lg border border-border bg-muted/40 p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-sm font-medium">
            {t("settings.cloudSync.contentTitle")}
          </h3>
          <p className="mt-1 text-xs text-muted-foreground">
            {t("settings.cloudSync.contentDescription")}
          </p>
        </div>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => void openManager()}
          disabled={!configured}
        >
          {t("settings.cloudSync.manageRemote")}
        </Button>
      </div>

      {stats?.paused && (
        <p className="text-xs text-amber-600 dark:text-amber-400">
          {t("settings.cloudSync.paused")}
        </p>
      )}
      {stats?.legacyCombined && (
        <p className="text-xs text-amber-600 dark:text-amber-400">
          {t("settings.cloudSync.legacyCombined")}
        </p>
      )}

      {loading && !stats ? (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
          {t("settings.cloudSync.loadingStats")}
        </div>
      ) : (
        <div className="space-y-2">
          {CATEGORY_ORDER.map((id) => {
            const row = stats?.categories.find((item) => item.category === id);
            const enabled = selectionRecord[id];
            return (
              <div
                key={id}
                className="flex items-start justify-between gap-3 rounded-md border border-border/60 bg-background/60 px-3 py-2"
                data-testid={`sync-category-${id}`}
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <p className="text-xs font-medium">
                      {t(`settings.cloudSync.categories.${id}.name`)}
                    </p>
                    <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
                      {t(`settings.cloudSync.status.${row?.status ?? (enabled ? "pending" : "disabled")}`)}
                    </span>
                  </div>
                  <p className="text-[11px] text-muted-foreground">
                    {t(`settings.cloudSync.categories.${id}.description`)}
                  </p>
                  {SENSITIVE_HINT.has(id) && (
                    <p className="mt-1 flex items-center gap-1 text-[11px] text-amber-600 dark:text-amber-400">
                      <ShieldAlert className="h-3 w-3" />
                      {t("settings.cloudSync.sensitiveHint")}
                    </p>
                  )}
                  <p className="mt-1 text-[11px] text-muted-foreground">
                    {id === "skill_files"
                      ? t("settings.cloudSync.skillFilesStats", {
                          files: row?.localFileCount ?? 0,
                          local: formatSyncBytes(row?.localUncompressedBytes),
                          remote: formatSyncBytes(row?.remoteBytes),
                        })
                      : t("settings.cloudSync.itemStats", {
                          count: row?.localItemCount ?? 0,
                          remote: formatSyncBytes(row?.remoteBytes),
                        })}
                    {row?.legacyCombined
                      ? ` · ${t("settings.cloudSync.legacyCombined")}`
                      : ""}
                    {row?.legacyCombined && enabled
                      ? ` · ${t("settings.cloudSync.needsMigration")}`
                      : ""}
                  </p>
                  {enabled && row?.status === "pending" && (
                    <Button
                      type="button"
                      size="sm"
                      variant="secondary"
                      className="mt-2 h-7 text-[11px]"
                      onClick={() => row && setPendingCategory(row)}
                    >
                      {t("settings.cloudSync.chooseDirection")}
                    </Button>
                  )}
                </div>
                <Switch
                  checked={enabled}
                  disabled={saving}
                  onCheckedChange={(checked) => void handleToggle(id, checked)}
                  aria-label={t(`settings.cloudSync.categories.${id}.name`)}
                />
              </div>
            );
          })}
        </div>
      )}

      <Dialog
        open={!!pendingCategory}
        onOpenChange={(open) => {
          if (!open) setPendingCategory(null);
        }}
      >
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>{t("settings.cloudSync.initTitle")}</DialogTitle>
            <DialogDescription>
              {t("settings.cloudSync.initDescription")}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex gap-2 sm:justify-end">
            <Button variant="outline" onClick={() => setPendingCategory(null)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="secondary"
              disabled={!pendingCategory || pendingCategory.status === "remote_missing"}
              onClick={() => void handleInit("download")}
            >
              {t("settings.cloudSync.initDownload")}
            </Button>
            <Button onClick={() => void handleInit("upload")}>
              {t("settings.cloudSync.initUpload")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={manageOpen} onOpenChange={setManageOpen}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>{t("settings.cloudSync.manageTitle")}</DialogTitle>
            <DialogDescription>
              {t("settings.cloudSync.manageDescription")}
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-80 space-y-2 overflow-auto text-xs">
            {inventory && (
              <p className="text-muted-foreground" data-testid="sync-remote-target">
                {t("settings.cloudSync.targetServer", {
                  root: inventory.displayRoot,
                  profile: inventory.profile,
                })}
              </p>
            )}
            {CATEGORY_ORDER.map((id) => {
              const enabled = selectionRecord[id];
              const remote = inventory?.v3?.categories?.[id];
              const selected = deleteTargets.includes(id);
              const deletable = !enabled && Boolean(remote);
              return (
                <label
                  key={id}
                  className="flex items-start justify-between gap-2 rounded border border-border/60 px-2 py-1.5"
                >
                  <span className="min-w-0 space-y-0.5">
                    <span className="block font-medium">
                      {t(`settings.cloudSync.categories.${id}.name`)}
                    </span>
                    <span className="block text-muted-foreground">
                      {remote
                        ? `${formatSyncBytes(remote.size)} · ${t("settings.cloudSync.itemCount", { count: remote.itemCount })} · ${remote.sha256.slice(0, 8)} · ${remote.deviceName}`
                        : t("settings.cloudSync.noRemoteCategory")}
                    </span>
                  </span>
                  <input
                    type="checkbox"
                    disabled={!deletable}
                    checked={selected}
                    onChange={(event) => {
                      setDeleteTargets((current) =>
                        event.target.checked
                          ? [...current, id]
                          : current.filter((item) => item !== id),
                      );
                    }}
                  />
                </label>
              );
            })}
            {renderV2Summary(inventory?.v2Current, "current", t)}
            {renderV2Summary(inventory?.v2Legacy, "legacy", t)}
            {!!(inventory?.target?.cleanupIncomplete ?? stats?.cleanupIncomplete)?.length && (
              <div className="space-y-1">
                <p className="font-medium">
                  {t("settings.cloudSync.leftoverArtifacts")}
                </p>
                {(inventory?.target?.cleanupIncomplete ?? stats?.cleanupIncomplete ?? []).map(
                  (item) => (
                    <p key={item.key} className="truncate text-muted-foreground">
                      {item.key}
                    </p>
                  ),
                )}
                <Button
                  size="sm"
                  variant="outline"
                  onClick={async () => {
                    try {
                      const report = await settingsApi.cloudSyncRetryCleanup();
                      handleReport(report, t);
                      await openManager();
                      await loadStats();
                    } catch (error) {
                      toast.error((error as Error)?.message ?? String(error));
                    }
                  }}
                >
                  {t("settings.cloudSync.retryCleanup")}
                </Button>
              </div>
            )}
          </div>
          <DialogFooter className="flex flex-wrap gap-2 sm:justify-end">
            <Button variant="outline" onClick={() => setManageOpen(false)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="secondary"
              disabled={!inventory?.v3}
              onClick={() => setConfirmV2(true)}
            >
              {t("settings.cloudSync.deleteV2")}
            </Button>
            <Button
              variant="destructive"
              disabled={deleteTargets.length === 0 || inventory?.supportsConditionalWrite === false}
              onClick={() => setConfirmDelete(true)}
            >
              {t("settings.cloudSync.deleteSelected")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={confirmDelete} onOpenChange={setConfirmDelete}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>{t("settings.cloudSync.deleteConfirmTitle")}</DialogTitle>
            <DialogDescription>
              {t("settings.cloudSync.deleteConfirmDetails", {
                names: deleteTargets
                  .map((id) => t(`settings.cloudSync.categories.${id}.name`))
                  .join(", "),
                size: formatSyncBytes(
                  deleteTargets.reduce(
                    (sum, id) =>
                      sum + (inventory?.v3?.categories?.[id]?.size ?? 0),
                    0,
                  ),
                ),
                root: inventory?.displayRoot ?? "",
                profile: inventory?.profile ?? "",
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmDelete(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="destructive" onClick={() => void handleDelete()}>
              {t("settings.cloudSync.deleteSelected")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={confirmV2} onOpenChange={setConfirmV2}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>{t("settings.cloudSync.deleteV2Title")}</DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-2">
                <p>{t("settings.cloudSync.deleteV2Message")}</p>
                {renderV2Summary(inventory?.v2Current, "current", t)}
                {renderV2Summary(inventory?.v2Legacy, "legacy", t)}
              </div>
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmV2(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="destructive" onClick={() => void handleDeleteV2()}>
              {t("settings.cloudSync.deleteV2")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

const SENSITIVE_HINT = new Set<SyncCategory>([
  "providers",
  "mcp",
  "common_config",
  "proxy_settings",
]);

function renderV2Summary(
  snapshot:
    | {
        artifacts?: Record<string, { sha256: string; size: number }>;
      }
    | null
    | undefined,
  layout: "current" | "legacy",
  t: (key: string, opts?: Record<string, unknown>) => string,
) {
  if (!snapshot?.artifacts || Object.keys(snapshot.artifacts).length === 0) {
    return null;
  }
  const total = Object.values(snapshot.artifacts).reduce(
    (sum, item) => sum + (item.size ?? 0),
    0,
  );
  return (
    <div className="rounded border border-border/60 px-2 py-1.5">
      <p className="font-medium">
        {t("settings.cloudSync.v2Snapshot", { layout })}
      </p>
      {Object.entries(snapshot.artifacts).map(([name, meta]) => (
        <p key={name} className="text-muted-foreground">
          {t("settings.cloudSync.v2Size", {
            name,
            size: formatSyncBytes(meta.size),
          })}
        </p>
      ))}
      <p className="text-muted-foreground">
        {t("settings.cloudSync.estimatedBytes", {
          size: formatSyncBytes(total),
        })}
      </p>
    </div>
  );
}
