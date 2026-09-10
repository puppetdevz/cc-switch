import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom";

import { SyncContentCard } from "@/components/settings/sync/SyncContentCard";
import {
  applyCategoryToggle,
  DEFAULT_CLOUD_SYNC_SELECTION,
  anyCategoryEnabled,
  describeSyncScope,
  formatSyncBytes,
} from "@/lib/syncCategories";
import { handleReport } from "@/lib/syncReports";
import type { CloudSyncStats, SyncCategory } from "@/types";

const toastSuccessMock = vi.fn();
const toastErrorMock = vi.fn();
const toastInfoMock = vi.fn();

vi.mock("sonner", () => ({
  toast: {
    success: (...args: unknown[]) => toastSuccessMock(...args),
    error: (...args: unknown[]) => toastErrorMock(...args),
    warning: vi.fn(),
    info: (...args: unknown[]) => toastInfoMock(...args),
  },
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string) => key,
  }),
}));

vi.mock("@/components/ui/button", () => ({
  Button: ({ children, ...props }: any) => <button {...props}>{children}</button>,
}));

vi.mock("@/components/ui/switch", () => ({
  Switch: ({ checked, onCheckedChange, ...props }: any) => (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onCheckedChange?.(!checked)}
      {...props}
    />
  ),
}));

vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ open, children }: any) => (open ? <div>{children}</div> : null),
  DialogContent: ({ children }: any) => <div>{children}</div>,
  DialogDescription: ({ children }: any) => <div>{children}</div>,
  DialogFooter: ({ children }: any) => <div>{children}</div>,
  DialogHeader: ({ children }: any) => <div>{children}</div>,
  DialogTitle: ({ children }: any) => <h2>{children}</h2>,
}));

const { settingsApiMock } = vi.hoisted(() => ({
  settingsApiMock: {
    cloudSyncGetCategoryStats: vi.fn(),
    cloudSyncSetSelection: vi.fn(),
    cloudSyncInitCategory: vi.fn(),
    cloudSyncDeleteCategories: vi.fn(),
    cloudSyncDeleteV2Snapshot: vi.fn(),
    cloudSyncRetryCleanup: vi.fn(),
    cloudSyncRemoteInventory: vi.fn(),
  },
}));

vi.mock("@/lib/api", () => ({
  settingsApi: settingsApiMock,
}));

function allEnabledSelection() {
  return {
    providers: true,
    mcp: true,
    prompts: true,
    skillRepos: true,
    skillMetadata: true,
    skillFiles: true,
    profiles: true,
    commonConfig: true,
    proxySettings: true,
    diagnosticsSettings: true,
    modelPricing: true,
  };
}

function statsWith(
  overrides: Partial<CloudSyncStats> = {},
  categoryOverrides: Partial<Record<SyncCategory, Partial<CloudSyncStats["categories"][number]>>> = {},
): CloudSyncStats {
  const categories: CloudSyncStats["categories"] = (
    [
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
    ] as SyncCategory[]
  ).map((category) => ({
    category,
    enabled: true,
    status: "ready" as const,
    localItemCount: 1,
    localBytes: 10,
    sensitive: ["providers", "mcp", "common_config", "proxy_settings"].includes(
      category,
    ),
    legacyCombined: false,
    ...categoryOverrides[category],
  }));
  return {
    selection: allEnabledSelection(),
    categories,
    paused: false,
    hasV3: true,
    legacyCombined: false,
    supportsConditionalWrite: true,
    cleanupIncomplete: [],
    ...overrides,
  };
}

describe("sync category helpers", () => {
  it("defaults A-K to enabled", () => {
    expect(Object.values(DEFAULT_CLOUD_SYNC_SELECTION).every(Boolean)).toBe(
      true,
    );
  });

  it("enabling skill files forces metadata", () => {
    const next = applyCategoryToggle(
      { ...DEFAULT_CLOUD_SYNC_SELECTION, skill_metadata: false, skill_files: false },
      "skill_files",
      true,
    );
    expect(next.skill_files).toBe(true);
    expect(next.skill_metadata).toBe(true);
  });

  it("disabling metadata also disables skill files", () => {
    const next = applyCategoryToggle(
      DEFAULT_CLOUD_SYNC_SELECTION,
      "skill_metadata",
      false,
    );
    expect(next.skill_metadata).toBe(false);
    expect(next.skill_files).toBe(false);
  });

  it("allows turning everything off", () => {
    let selection = { ...DEFAULT_CLOUD_SYNC_SELECTION };
    for (const key of Object.keys(selection) as Array<keyof typeof selection>) {
      selection = applyCategoryToggle(selection, key, false);
    }
    expect(anyCategoryEnabled(selection)).toBe(false);
  });
});

describe("SyncContentCard", () => {
  beforeEach(() => {
    toastSuccessMock.mockReset();
    toastErrorMock.mockReset();
    toastInfoMock.mockReset();
    Object.values(settingsApiMock).forEach((fn) => fn.mockReset());
    settingsApiMock.cloudSyncGetCategoryStats.mockResolvedValue(statsWith());
    settingsApiMock.cloudSyncSetSelection.mockImplementation(async (sel) => sel);
    settingsApiMock.cloudSyncRemoteInventory.mockResolvedValue({
      selection: allEnabledSelection(),
      target: { fingerprint: "fp", cleanupIncomplete: [] },
      v3: {
        snapshotId: "snap",
        deviceName: "Mac",
        createdAt: "2026-09-05T00:00:00Z",
        protocolVersion: 3,
        categories: {
          prompts: {
            schemaVersion: 1,
            artifact: "artifacts/prompts/abc.json",
            sha256: "abcdef123456",
            size: 42,
            itemCount: 2,
            updatedAt: "2026-09-05T00:00:00Z",
            deviceName: "Mac",
          },
        },
      },
      v2Current: {
        artifacts: {
          "db.sql": { sha256: "d", size: 100 },
          "skills.zip": { sha256: "s", size: 200 },
        },
      },
      displayRoot: "dav.example.com/sync",
      profile: "default",
      remoteRoot: "sync",
      supportsConditionalWrite: true,
    });
    settingsApiMock.cloudSyncDeleteCategories.mockResolvedValue({
      status: "success",
      categories: [],
      warnings: [],
    });
    settingsApiMock.cloudSyncDeleteV2Snapshot.mockResolvedValue({
      status: "success",
      categories: [],
      warnings: [],
    });
    settingsApiMock.cloudSyncInitCategory.mockResolvedValue({
      status: "success",
      categories: [],
      warnings: [],
    });
  });

  it("renders all A-K categories checked by default", async () => {
    render(<SyncContentCard configured />);
    await waitFor(() => {
      expect(screen.getByTestId("sync-category-providers")).toBeInTheDocument();
    });
    for (const id of [
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
    ]) {
      const row = screen.getByTestId(`sync-category-${id}`);
      expect(row.querySelector('[role="switch"]')).toHaveAttribute(
        "aria-checked",
        "true",
      );
    }
  });

  it("shows paused copy when nothing is selected", async () => {
    settingsApiMock.cloudSyncGetCategoryStats.mockResolvedValue(
      statsWith({
        paused: true,
        selection: {
          providers: false,
          mcp: false,
          prompts: false,
          skillRepos: false,
          skillMetadata: false,
          skillFiles: false,
          profiles: false,
          commonConfig: false,
          proxySettings: false,
          diagnosticsSettings: false,
          modelPricing: false,
        },
      }),
    );
    render(<SyncContentCard configured />);
    await waitFor(() => {
      expect(screen.getByText("settings.cloudSync.paused")).toBeInTheDocument();
    });
  });

  it("shows pending direction action", async () => {
    settingsApiMock.cloudSyncGetCategoryStats.mockResolvedValue(
      statsWith({}, { prompts: { status: "pending" } }),
    );
    render(<SyncContentCard configured />);
    await waitFor(() => {
      expect(
        screen.getByText("settings.cloudSync.chooseDirection"),
      ).toBeInTheDocument();
    });
    fireEvent.click(screen.getByText("settings.cloudSync.chooseDirection"));
    expect(screen.getByText("settings.cloudSync.initUpload")).toBeInTheDocument();
  });

  it("only allows deleting disabled categories in remote manager", async () => {
    settingsApiMock.cloudSyncGetCategoryStats.mockResolvedValue(
      statsWith({
        selection: {
          ...allEnabledSelection(),
          prompts: false,
        },
      }),
    );
    render(<SyncContentCard configured />);
    await waitFor(() => {
      expect(
        screen.getByText("settings.cloudSync.manageRemote"),
      ).toBeInTheDocument();
    });
    fireEvent.click(screen.getByText("settings.cloudSync.manageRemote"));
    await waitFor(() => {
      expect(screen.getByTestId("sync-remote-target")).toBeInTheDocument();
    });
    const boxes = screen.getAllByRole("checkbox") as HTMLInputElement[];
    const promptsBox = boxes.find((box) => !box.disabled);
    expect(promptsBox).toBeTruthy();
    expect(boxes.filter((box) => box.disabled).length).toBeGreaterThan(0);
    expect(screen.getByTestId("sync-remote-target")).toHaveTextContent(
      "settings.cloudSync.targetServer",
    );
    expect(
      screen.getByText("settings.cloudSync.v2Snapshot"),
    ).toBeInTheDocument();
  });

  it("does not toast success on conflict", () => {
    handleReport(
      { status: "conflict", categories: [], warnings: [] },
      (key) => key,
    );
    expect(toastErrorMock).toHaveBeenCalledWith("settings.cloudSync.conflict");
    expect(toastSuccessMock).not.toHaveBeenCalled();
  });
});

describe("sync scope helpers", () => {
  it("lists pending and disabled categories as skipped", () => {
    const scope = describeSyncScope(
      {
        categories: [
          {
            category: "providers",
            enabled: true,
            status: "ready",
            localBytes: 10,
            remoteBytes: 20,
            sensitive: true,
          },
          {
            category: "prompts",
            enabled: true,
            status: "pending",
            localBytes: 5,
            remoteBytes: 8,
            sensitive: false,
          },
          {
            category: "mcp",
            enabled: false,
            status: "disabled",
            localBytes: 1,
            remoteBytes: 1,
            sensitive: true,
          },
        ],
      },
      "download",
    );
    expect(scope.participating.map((row) => row.category)).toEqual([
      "providers",
    ]);
    expect(scope.skipped.map((row) => row.category)).toEqual([
      "prompts",
      "mcp",
    ]);
    expect(scope.estimated).toBe(20);
    expect(scope.sensitive).toBe(true);
  });

  it("formats zero bytes instead of treating them as missing", () => {
    expect(formatSyncBytes(0)).toBe("0 B");
  });
});
