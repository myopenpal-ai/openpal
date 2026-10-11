import { useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { useDialogFocus } from "../components/useDialogFocus";
import type { Skill } from "../lib/types";
import { useStore } from "../state/store";

const emptyDraft: Skill = { slug: "", name: "", description: "", body: "" };

type ImportKind = "file" | "folder" | "zip";

export default function SkillsView() {
  const { t } = useTranslation();
  const skills = useStore((s) => s.skills);
  const saveSkill = useStore((s) => s.saveSkill);
  const deleteSkill = useStore((s) => s.deleteSkill);
  const importSkillFile = useStore((s) => s.importSkillFile);
  const importSkillFolder = useStore((s) => s.importSkillFolder);
  const importSkillZip = useStore((s) => s.importSkillZip);
  const exportSkill = useStore((s) => s.exportSkill);
  const [editing, setEditing] = useState<Skill | null>(null);
  const [saving, setSaving] = useState(false);
  const [deletingSlug, setDeletingSlug] = useState<string | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  const [importingKind, setImportingKind] = useState<ImportKind | null>(null);
  const [importError, setImportError] = useState<{ kind: ImportKind; message: string } | null>(
    null,
  );

  const close = () => setEditing(null);
  const titleId = useId();
  const dialogRef = useDialogFocus<HTMLDivElement>(close);

  // Bumped by `closeImport` to invalidate whatever `runImport` call is
  // currently in flight, so a picker result that resolves *after* the dialog
  // was closed (and possibly reopened for a fresh attempt) can't resurrect a
  // stale error or spinner into that new session — see `runImport`.
  const importRunId = useRef(0);

  const closeImport = () => {
    importRunId.current += 1;
    setImportOpen(false);
    setImportingKind(null);
    setImportError(null);
  };
  const importTitleId = useId();
  const importDialogRef = useDialogFocus<HTMLDivElement>(closeImport);

  const save = async () => {
    if (!editing || editing.name.trim() === "" || editing.body.trim() === "") return;
    setSaving(true);
    const result = await saveSkill(editing);
    setSaving(false);
    if (result) close();
  };

  const runImport = async (kind: ImportKind) => {
    const action =
      kind === "file" ? importSkillFile : kind === "folder" ? importSkillFolder : importSkillZip;
    const runId = importRunId.current;
    setImportError(null);
    setImportingKind(kind);
    try {
      const result = await action();
      if (importRunId.current !== runId) return; // dialog closed/reopened meanwhile
      setImportingKind(null);
      // `null` just means the user closed the native picker without
      // choosing anything — not an error, leave the modal open either way.
      if (result) closeImport();
    } catch (err) {
      if (importRunId.current !== runId) return; // dialog closed/reopened meanwhile
      setImportingKind(null);
      setImportError({ kind, message: err instanceof Error ? err.message : String(err) });
    }
  };

  return (
    <div className="skills-view">
      <div className="skills-scroll">
        <div className="skills-col">
          <header className="skills-head">
            <div>
              <h1>{t("skillsView.title")}</h1>
              <p>
                {t("skillsView.descriptionBefore")}
                <code>/</code>
                {t("skillsView.descriptionAfter")}
              </p>
            </div>
            <div className="skills-head-actions">
              <button className="btn-ghost" onClick={() => setImportOpen(true)}>
                {t("skillsView.import")}
              </button>
              <button className="btn" onClick={() => setEditing(emptyDraft)}>
                {t("skillsView.newSkill")}
              </button>
            </div>
          </header>

          {skills.length === 0 ? (
            <div className="skills-empty">
              <p>{t("skillsView.emptyState")}</p>
            </div>
          ) : (
            <div className="skill-grid">
              {skills.map((skill) => (
                <article key={skill.slug} className="skill-card">
                  <div className="skill-card-head">
                    <h2>{skill.name}</h2>
                    <div className="skill-card-badges">
                      <code className="skill-trigger">/{skill.slug}</code>
                      {skill.files && skill.files.length > 0 && (
                        <span className="skill-file-badge">
                          +{t("skillsView.fileCount", { count: skill.files.length })}
                        </span>
                      )}
                    </div>
                  </div>
                  {skill.description && <p className="skill-desc">{skill.description}</p>}
                  {deletingSlug === skill.slug ? (
                    <div className="skill-delete-confirm">
                      <p className="skill-delete-confirm-text">
                        {t("skillsView.deleteConfirm", { name: skill.name })}
                      </p>
                      <div className="skill-card-actions">
                        <button
                          className="btn-quiet"
                          onClick={() => setDeletingSlug(null)}
                        >
                          {t("common.cancel")}
                        </button>
                        <button
                          className="btn-danger-quiet"
                          onClick={() => {
                            setDeletingSlug(null);
                            void deleteSkill(skill.slug);
                          }}
                        >
                          {t("skillsView.delete")}
                        </button>
                      </div>
                    </div>
                  ) : (
                    <div className="skill-card-actions">
                      <button className="btn-quiet" onClick={() => setEditing(skill)}>
                        {t("skillsView.edit")}
                      </button>
                      <button className="btn-quiet" onClick={() => void exportSkill(skill.slug)}>
                        {t("skillsView.export")}
                      </button>
                      <button
                        className="btn-danger-quiet"
                        onClick={() => setDeletingSlug(skill.slug)}
                      >
                        {t("skillsView.delete")}
                      </button>
                    </div>
                  )}
                </article>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Both dialogs portal to <body>, like ConfirmDialog: this page lives
          inside the More dialog, and a dialog stacked on it must layer over
          that one rather than render inside its box. */}
      {editing &&
        createPortal(
          <div className="modal-backdrop" onClick={close}>
            <div
              ref={dialogRef}
              tabIndex={-1}
              className="modal"
              role="dialog"
              aria-modal="true"
              aria-labelledby={titleId}
              onClick={(e) => e.stopPropagation()}
            >
              <h2 id={titleId}>
                {editing.slug ? t("skillsView.editSkillTitle") : t("skillsView.newSkillTitle")}
              </h2>
              {editing.slug && (
                <p className="settings-sub">
                  {t("skillsView.triggeredByBefore")}
                  <code>/{editing.slug}</code>
                  {t("skillsView.triggeredByAfter")}
                </p>
              )}
              {editing.files && editing.files.length > 0 && (
                <p className="skill-files-note">
                  {t("skillsView.supportingFilesNote", {
                    count: editing.files.length,
                    files: editing.files.map((f) => f.path).join(", "),
                  })}
                </p>
              )}
              <label className="skill-field">
                {t("skillsView.nameLabel")}
                <input
                  type="text"
                  value={editing.name}
                  autoFocus
                  onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                  placeholder={t("skillsView.namePlaceholder")}
                />
              </label>
              <label className="skill-field">
                {t("skillsView.descriptionLabel")}
                <input
                  type="text"
                  value={editing.description}
                  onChange={(e) => setEditing({ ...editing, description: e.target.value })}
                  placeholder={t("skillsView.descriptionPlaceholder")}
                />
              </label>
              <label className="skill-field">
                {t("skillsView.instructionsLabel")}
                <textarea
                  className="skill-body"
                  rows={10}
                  value={editing.body}
                  onChange={(e) => setEditing({ ...editing, body: e.target.value })}
                  placeholder={t("skillsView.instructionsPlaceholder")}
                />
              </label>
              <div className="update-actions">
                <div />
                <div className="update-actions-right">
                  <button className="btn-ghost" onClick={close} disabled={saving}>
                    {t("common.cancel")}
                  </button>
                  <button
                    className="btn"
                    onClick={() => void save()}
                    disabled={saving || editing.name.trim() === "" || editing.body.trim() === ""}
                  >
                    {saving ? t("skillsView.saving") : t("common.save")}
                  </button>
                </div>
              </div>
            </div>
          </div>,
          document.body,
        )}

      {importOpen &&
        createPortal(
          <div className="modal-backdrop" onClick={closeImport}>
            <div
              ref={importDialogRef}
              tabIndex={-1}
              className="modal"
              role="dialog"
              aria-modal="true"
              aria-labelledby={importTitleId}
              onClick={(e) => e.stopPropagation()}
            >
              <h2 id={importTitleId}>{t("skillsView.importTitle")}</h2>
              <p className="settings-sub">{t("skillsView.importSubtitle")}</p>

              <div className="skill-import-options">
                <div className="skill-import-option">
                  <div className="skill-import-option-text">
                    <h3>{t("skillsView.importFileOption")}</h3>
                    <p>{t("skillsView.importFileDesc")}</p>
                    {importError?.kind === "file" && (
                      <p className="skill-import-error">{importError.message}</p>
                    )}
                  </div>
                  <button
                    className="btn-quiet"
                    onClick={() => void runImport("file")}
                    disabled={importingKind !== null}
                  >
                    {importingKind === "file" ? t("skillsView.importing") : t("skillsView.importChoose")}
                  </button>
                </div>

                <div className="skill-import-option">
                  <div className="skill-import-option-text">
                    <h3>{t("skillsView.importFolderOption")}</h3>
                    <p>{t("skillsView.importFolderDesc")}</p>
                    {importError?.kind === "folder" && (
                      <p className="skill-import-error">{importError.message}</p>
                    )}
                  </div>
                  <button
                    className="btn-quiet"
                    onClick={() => void runImport("folder")}
                    disabled={importingKind !== null}
                  >
                    {importingKind === "folder" ? t("skillsView.importing") : t("skillsView.importChoose")}
                  </button>
                </div>

                <div className="skill-import-option">
                  <div className="skill-import-option-text">
                    <h3>{t("skillsView.importZipOption")}</h3>
                    <p>{t("skillsView.importZipDesc")}</p>
                    {importError?.kind === "zip" && (
                      <p className="skill-import-error">{importError.message}</p>
                    )}
                  </div>
                  <button
                    className="btn-quiet"
                    onClick={() => void runImport("zip")}
                    disabled={importingKind !== null}
                  >
                    {importingKind === "zip" ? t("skillsView.importing") : t("skillsView.importChoose")}
                  </button>
                </div>
              </div>

              <div className="update-actions">
                <div />
                <div className="update-actions-right">
                  {/* Deliberately never disabled, even mid-import: closing just
                      invalidates that import's result (see `importRunId`) —
                      it doesn't need to finish first. Also keeps at least one
                      focusable control in the dialog while the three "Choose…"
                      buttons are disabled, so the focus trap below always has
                      somewhere to land instead of losing focus to the page
                      behind it. */}
                  <button className="btn-ghost" onClick={closeImport}>
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}
