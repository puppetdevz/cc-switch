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
