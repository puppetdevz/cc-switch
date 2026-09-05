export const SYNC_CATEGORIES = [
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
] as const;

export type SyncCategoryId = (typeof SYNC_CATEGORIES)[number];

export const SENSITIVE_CATEGORIES: SyncCategoryId[] = [
  "providers",
  "mcp",
  "common_config",
  "proxy_settings",
];

export const DEFAULT_CLOUD_SYNC_SELECTION: Record<SyncCategoryId, boolean> =
  Object.fromEntries(SYNC_CATEGORIES.map((id) => [id, true])) as Record<
    SyncCategoryId,
    boolean
  >;

export function normalizeSkillDependency(
  selection: Record<SyncCategoryId, boolean>,
): Record<SyncCategoryId, boolean> {
  const next = { ...selection };
  if (next.skill_files) next.skill_metadata = true;
  if (!next.skill_metadata) next.skill_files = false;
  return next;
}

export function applyCategoryToggle(
  selection: Record<SyncCategoryId, boolean>,
  category: SyncCategoryId,
  enabled: boolean,
): Record<SyncCategoryId, boolean> {
  const next = { ...selection, [category]: enabled };
  if (category === "skill_files" && enabled) next.skill_metadata = true;
  if (category === "skill_metadata" && !enabled) next.skill_files = false;
  return normalizeSkillDependency(next);
}

export function anyCategoryEnabled(
  selection: Record<SyncCategoryId, boolean>,
): boolean {
  return SYNC_CATEGORIES.some((id) => selection[id]);
}

export function formatSyncBytes(bytes?: number | null): string {
  if (bytes == null) return "—";
  if (bytes === 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function describeSyncScope(
  stats: {
    categories: Array<{
      category: SyncCategoryId;
      enabled: boolean;
      status: string;
      localBytes: number;
      remoteBytes?: number | null;
      sensitive: boolean;
    }>;
    legacyCombined?: boolean;
  } | null,
  kind: "upload" | "download",
) {
  const rows = stats?.categories ?? [];
  const participating = rows.filter(
    (row) =>
      row.enabled && row.status !== "pending" && row.status !== "disabled",
  );
  const skipped = rows.filter(
    (row) =>
      !row.enabled || row.status === "pending" || row.status === "disabled",
  );
  const estimated = participating.reduce((sum, row) => {
    const size =
      kind === "download" ? (row.remoteBytes ?? 0) : row.localBytes;
    return sum + size;
  }, 0);
  return {
    participating,
    skipped,
    estimated,
    sensitive: participating.some((row) => row.sensitive),
    legacyCombined: Boolean(stats?.legacyCombined),
  };
}
