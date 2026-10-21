import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import { AutoBackupField, YourDataSection } from "./SettingsSheet";
import { useStore } from "../state/store";
import type { AppInfo, BackupState, ImportSummary } from "../lib/types";

const api = vi.hoisted(() => ({
  getBackupState: vi.fn(),
  setBackupEnabled: vi.fn(),
  setBackupFrequency: vi.fn(),
  chooseBackupFolder: vi.fn(),
  pickBackupFile: vi.fn(),
  restoreBackup: vi.fn(),
  onBackupStateChanged: vi.fn(),
  getAppInfo: vi.fn(),
  // The rest of Your data, for the section-level tests.
  estimateExportSize: vi.fn(),
  exportCounts: vi.fn(),
}));

vi.mock("../lib/api", () => ({ api }));

const off: BackupState = {
  enabled: false,
  folder: null,
  frequency: "daily",
  lastSuccessMs: null,
  lastAttemptMs: null,
  lastError: null,
};

const appInfo = (overrides: Partial<AppInfo> = {}) =>
  ({
    portable: false,
    isAndroid: false,
    needsRecoverySetup: false,
    dataDir: "/data",
    ...overrides,
  }) as AppInfo;

const summary: ImportSummary = {
  conversations: 4,
  projects: 2,
  artifacts: 3,
  skills: 1,
  customModels: 0,
  settings: true,
  plugins: 1,
  skipped: 0,
};

const reloadAfterImport = vi.fn().mockResolvedValue(undefined);
let fireBackupStateChanged: () => void = () => {};

beforeEach(() => {
  for (const fn of Object.values(api)) fn.mockReset();
  api.getBackupState.mockResolvedValue(off);
  api.onBackupStateChanged.mockImplementation(async (cb: () => void) => {
    fireBackupStateChanged = cb;
    return () => {};
  });
  api.estimateExportSize.mockResolvedValue(1024);
  api.exportCounts.mockResolvedValue(null);
  reloadAfterImport.mockClear();
  useStore.setState({ appInfo: appInfo(), reloadAfterImport });
});

const toggle = () => screen.getByRole("checkbox");

describe("AutoBackupField", () => {
  it("is off by default and every day is the default frequency", async () => {
    render(<AutoBackupField />);
    await waitFor(() => expect(toggle()).toBeEnabled());
    expect(toggle()).not.toBeChecked();
    expect(screen.getByRole("combobox", { name: "How often" })).toHaveValue("daily");
  });

  it("turning it on asks for a folder first, then switches on", async () => {
    const folder = "/Volumes/Backup";
    api.chooseBackupFolder.mockResolvedValue({ ...off, folder });
    api.setBackupEnabled.mockResolvedValue({ ...off, folder, enabled: true });
    render(<AutoBackupField />);
    await waitFor(() => expect(toggle()).toBeEnabled());

    await userEvent.click(toggle());

    expect(api.chooseBackupFolder).toHaveBeenCalledOnce();
    expect(api.setBackupEnabled).toHaveBeenCalledWith(true);
    expect(toggle()).toBeChecked();
    expect(screen.getByText(folder)).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("No backup yet");
  });

  it("stays off when the folder picker is closed", async () => {
    api.chooseBackupFolder.mockResolvedValue(null);
    render(<AutoBackupField />);
    await waitFor(() => expect(toggle()).toBeEnabled());

    await userEvent.click(toggle());

    expect(api.setBackupEnabled).not.toHaveBeenCalled();
    expect(toggle()).not.toBeChecked();
  });

  it("turns off without touching the folder", async () => {
    api.getBackupState.mockResolvedValue({ ...off, enabled: true, folder: "/b" });
    api.setBackupEnabled.mockResolvedValue({ ...off, folder: "/b" });
    render(<AutoBackupField />);
    await waitFor(() => expect(toggle()).toBeChecked());

    await userEvent.click(toggle());

    expect(api.chooseBackupFolder).not.toHaveBeenCalled();
    expect(api.setBackupEnabled).toHaveBeenCalledWith(false);
    expect(toggle()).not.toBeChecked();
  });

  it("changes folder and frequency", async () => {
    api.getBackupState.mockResolvedValue({ ...off, enabled: true, folder: "/old" });
    api.chooseBackupFolder.mockResolvedValue({ ...off, enabled: true, folder: "/new" });
    api.setBackupFrequency.mockResolvedValue({
      ...off,
      enabled: true,
      folder: "/new",
      frequency: "weekly",
    });
    render(<AutoBackupField />);
    await screen.findByText("/old");

    await userEvent.click(screen.getByRole("button", { name: "Choose folder…" }));
    expect(await screen.findByText("/new")).toBeInTheDocument();

    await userEvent.selectOptions(screen.getByRole("combobox", { name: "How often" }), "weekly");
    expect(api.setBackupFrequency).toHaveBeenCalledWith("weekly");
    expect(screen.getByRole("combobox", { name: "How often" })).toHaveValue("weekly");
  });

  it("shows when the last backup was, and why the last one failed", async () => {
    api.getBackupState.mockResolvedValue({
      ...off,
      enabled: true,
      folder: "/b",
      lastSuccessMs: Date.now() - 30_000,
    });
    render(<AutoBackupField />);
    expect(await screen.findByRole("status")).toHaveTextContent(/^Last backup: /);

    api.getBackupState.mockResolvedValue({
      ...off,
      enabled: true,
      folder: "/b",
      lastAttemptMs: Date.now(),
      lastError: "The backup folder isn't available. Check that its drive is connected.",
    });
    act(() => fireBackupStateChanged());
    expect(
      await screen.findByText(
        "The last backup failed: The backup folder isn't available. Check that its drive is connected.",
      ),
    ).toBeInTheDocument();
  });

  it("can't be turned on before a recovery code exists, and leads to setting one up", async () => {
    useStore.setState({ appInfo: appInfo({ needsRecoverySetup: true }) });
    api.getAppInfo.mockResolvedValue(appInfo({ needsRecoverySetup: true }));
    render(<AutoBackupField />);
    await waitFor(() => expect(api.getBackupState).toHaveBeenCalled());

    expect(toggle()).toBeDisabled();
    expect(
      screen.getByText(
        "Set up your recovery code first. It's what unlocks a backup on a new computer.",
      ),
    ).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Set up recovery code" }));
    expect(api.getAppInfo).toHaveBeenCalledOnce();
    expect(useStore.getState().appInfo?.needsRecoverySetup).toBe(true);
  });

  it("restores a picked backup with the recovery code", async () => {
    api.pickBackupFile.mockResolvedValue("/b/openpal-backup-1.opbackup");
    api.restoreBackup.mockResolvedValue({ kind: "restored", summary });
    render(<AutoBackupField />);

    await userEvent.click(screen.getByRole("button", { name: "Restore from backup…" }));
    const code = screen.getByRole("textbox", {
      name: "Enter your recovery code to unlock this backup.",
    });
    expect(screen.getByRole("button", { name: "Restore" })).toBeDisabled();
    await userEvent.type(code, "ABCDE-FGHJK");
    await userEvent.click(screen.getByRole("button", { name: "Restore" }));

    expect(api.restoreBackup).toHaveBeenCalledWith("/b/openpal-backup-1.opbackup", "ABCDE-FGHJK");
    expect(reloadAfterImport).toHaveBeenCalledWith(summary);
    expect(
      await screen.findByText("Restored 4 chat(s), 2 project(s), and 3 file(s)."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("says so when the recovery code is wrong, and changes nothing", async () => {
    api.pickBackupFile.mockResolvedValue("/b/openpal-backup-1.opbackup");
    api.restoreBackup.mockResolvedValue({ kind: "wrongCode" });
    render(<AutoBackupField />);

    await userEvent.click(screen.getByRole("button", { name: "Restore from backup…" }));
    await userEvent.type(screen.getByRole("textbox"), "WRONG");
    await userEvent.click(screen.getByRole("button", { name: "Restore" }));

    expect(
      await screen.findByText("That recovery code doesn't unlock this backup."),
    ).toBeInTheDocument();
    expect(reloadAfterImport).not.toHaveBeenCalled();
    // The prompt stays open for another try.
    expect(screen.getByRole("textbox")).toHaveValue("WRONG");
  });

  it("shows a damaged backup's error", async () => {
    api.pickBackupFile.mockResolvedValue("/b/broken.opbackup");
    api.restoreBackup.mockRejectedValue("This backup file is damaged or incomplete.");
    render(<AutoBackupField />);

    await userEvent.click(screen.getByRole("button", { name: "Restore from backup…" }));
    await userEvent.type(screen.getByRole("textbox"), "CODE");
    await userEvent.click(screen.getByRole("button", { name: "Restore" }));

    expect(
      await screen.findByText("This backup file is damaged or incomplete."),
    ).toBeInTheDocument();
    expect(reloadAfterImport).not.toHaveBeenCalled();
  });
});

describe("YourDataSection's automatic backup", () => {
  it("shows on a normal install", async () => {
    render(<YourDataSection />);
    expect(await screen.findByText("Automatic backup")).toBeInTheDocument();
  });

  it.each([
    ["the portable build", appInfo({ portable: true })],
    ["Android", appInfo({ isAndroid: true })],
  ])("is hidden in %s", async (_, info) => {
    useStore.setState({ appInfo: info });
    render(<YourDataSection />);
    await screen.findByText("Export your data");
    expect(screen.queryByText("Automatic backup")).not.toBeInTheDocument();
    expect(screen.queryByText("Restore from backup…")).not.toBeInTheDocument();
  });
});
