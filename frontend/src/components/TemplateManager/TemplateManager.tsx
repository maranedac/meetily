"use client";

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { ConfirmationModal } from "@/components/ConfirmationModel/confirmation-modal";
import { Plus, Pencil, Trash2, Loader2, FileText, Lock } from "lucide-react";
import { TemplateEditorForm } from "./TemplateEditorForm";
import type { TemplateFull, TemplateInfo } from "./types";

interface TemplateManagerProps {
  /** Called after a template is created, edited, or deleted */
  onTemplatesChanged?: () => void;
}

type ViewState =
  | { mode: "list" }
  | { mode: "create" }
  | { mode: "edit"; template: TemplateFull };

export function TemplateManager({ onTemplatesChanged }: TemplateManagerProps) {
  const [templates, setTemplates] = useState<TemplateInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [view, setView] = useState<ViewState>({ mode: "list" });
  const [loadingTemplateId, setLoadingTemplateId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<TemplateInfo | null>(null);
  const [deleting, setDeleting] = useState(false);

  const fetchTemplates = useCallback(async () => {
    setLoading(true);
    try {
      const list = await invoke<TemplateInfo[]>("api_list_templates");
      setTemplates(list);
    } catch (error) {
      toast.error("Failed to load templates", {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchTemplates();
  }, [fetchTemplates]);

  const openEdit = async (info: TemplateInfo) => {
    setLoadingTemplateId(info.id);
    try {
      const full = await invoke<TemplateFull>("api_get_template_full", {
        templateId: info.id,
      });
      setView({ mode: "edit", template: full });
    } catch (error) {
      toast.error("Failed to load template", {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setLoadingTemplateId(null);
    }
  };

  const handleSaved = async (_info: TemplateInfo) => {
    setView({ mode: "list" });
    await fetchTemplates();
    onTemplatesChanged?.();
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await invoke("api_delete_template", { templateId: deleteTarget.id });
      toast.success("Template deleted", { description: `"${deleteTarget.name}" was removed` });
      setDeleteTarget(null);
      await fetchTemplates();
      onTemplatesChanged?.();
    } catch (error) {
      toast.error("Failed to delete template", {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setDeleting(false);
    }
  };

  if (view.mode === "create") {
    return (
      <div>
        <h3 className="text-base font-semibold mb-4">New template</h3>
        <TemplateEditorForm onCancel={() => setView({ mode: "list" })} onSaved={handleSaved} />
      </div>
    );
  }

  if (view.mode === "edit") {
    return (
      <div>
        <h3 className="text-base font-semibold mb-4">Edit "{view.template.name}"</h3>
        <TemplateEditorForm
          initial={view.template}
          onCancel={() => setView({ mode: "list" })}
          onSaved={handleSaved}
        />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">
          Templates control the sections and instructions used when generating an AI summary.
        </p>
        <Button size="sm" onClick={() => setView({ mode: "create" })}>
          <Plus className="h-4 w-4" />
          New template
        </Button>
      </div>

      {loading ? (
        <div className="flex items-center justify-center py-10 text-muted-foreground">
          <Loader2 className="h-5 w-5 animate-spin mr-2" />
          Loading templates...
        </div>
      ) : templates.length === 0 ? (
        <div className="text-sm text-muted-foreground py-10 text-center">
          No templates found.
        </div>
      ) : (
        <Accordion type="single" collapsible className="border rounded-md px-3">
          {templates.map((template) => (
            <AccordionItem key={template.id} value={template.id}>
              <div className="flex items-center gap-2">
                <AccordionTrigger className="flex-1">
                  <div className="flex items-center gap-2 text-left">
                    <FileText className="h-4 w-4 shrink-0 text-muted-foreground" />
                    <div>
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{template.name}</span>
                        <span
                          className={`text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded-full font-semibold ${
                            template.is_custom
                              ? "bg-blue-100 text-blue-700"
                              : "bg-gray-100 text-gray-500"
                          }`}
                        >
                          {template.is_custom ? "Custom" : "Built-in"}
                        </span>
                      </div>
                      <p className="text-xs text-muted-foreground font-normal">
                        {template.description}
                      </p>
                    </div>
                  </div>
                </AccordionTrigger>

                <div className="flex items-center gap-1 shrink-0">
                  {template.is_custom ? (
                    <>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        title="Edit template"
                        disabled={loadingTemplateId === template.id}
                        onClick={() => openEdit(template)}
                      >
                        {loadingTemplateId === template.id ? (
                          <Loader2 className="h-4 w-4 animate-spin" />
                        ) : (
                          <Pencil className="h-4 w-4" />
                        )}
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        title="Delete template"
                        className="text-red-600 hover:text-red-700 hover:bg-red-50"
                        onClick={() => setDeleteTarget(template)}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </>
                  ) : (
                    <span title="Built-in templates can't be edited or deleted" className="p-2 text-muted-foreground">
                      <Lock className="h-4 w-4" />
                    </span>
                  )}
                </div>
              </div>

              <AccordionContent>
                <TemplateSectionPreview templateId={template.id} />
              </AccordionContent>
            </AccordionItem>
          ))}
        </Accordion>
      )}

      <ConfirmationModal
        isOpen={deleteTarget !== null}
        text={
          deleteTarget
            ? `Are you sure you want to delete the "${deleteTarget.name}" template? This action cannot be undone.`
            : ""
        }
        onConfirm={confirmDelete}
        onCancel={() => (deleting ? null : setDeleteTarget(null))}
      />
    </div>
  );
}

/** Lazily loads and displays the full sections of a template when its accordion item is expanded */
function TemplateSectionPreview({ templateId }: { templateId: string }) {
  const [details, setDetails] = useState<TemplateFull | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    invoke<TemplateFull>("api_get_template_full", { templateId })
      .then((full) => {
        if (!cancelled) setDetails(full);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [templateId]);

  if (loading) {
    return (
      <div className="flex items-center text-sm text-muted-foreground py-2">
        <Loader2 className="h-4 w-4 animate-spin mr-2" />
        Loading sections...
      </div>
    );
  }

  if (error || !details) {
    return <p className="text-sm text-red-600">{error ?? "Failed to load template"}</p>;
  }

  return (
    <div className="space-y-3">
      {details.sections.map((section, i) => (
        <div key={i} className="rounded-md bg-muted/40 p-3">
          <div className="flex items-center gap-2 mb-1">
            <span className="font-medium text-sm">{section.title}</span>
            <span className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded-full bg-gray-200 text-gray-600 font-semibold">
              {section.format}
            </span>
          </div>
          <p className="text-sm text-muted-foreground">{section.instruction}</p>
          {section.item_format && (
            <p className="text-xs text-muted-foreground mt-1 font-mono">
              Item format: {section.item_format}
            </p>
          )}
        </div>
      ))}
    </div>
  );
}
