"use client";

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Plus, Trash2, Loader2 } from "lucide-react";
import type { TemplateFull, TemplateInfo, TemplateSectionDto } from "./types";

interface TemplateEditorFormProps {
  /** Pass an existing template to edit it; omit to create a new one */
  initial?: TemplateFull | null;
  onCancel: () => void;
  onSaved: (info: TemplateInfo) => void;
}

const emptySection = (): TemplateSectionDto => ({
  title: "",
  instruction: "",
  format: "paragraph",
  item_format: "",
});

export function TemplateEditorForm({ initial, onCancel, onSaved }: TemplateEditorFormProps) {
  const [name, setName] = useState(initial?.name ?? "");
  const [description, setDescription] = useState(initial?.description ?? "");
  const [sections, setSections] = useState<TemplateSectionDto[]>(
    initial?.sections?.length ? initial.sections : [emptySection()]
  );
  const [saving, setSaving] = useState(false);

  const isEditing = Boolean(initial);

  const updateSection = (index: number, patch: Partial<TemplateSectionDto>) => {
    setSections((prev) => prev.map((s, i) => (i === index ? { ...s, ...patch } : s)));
  };

  const addSection = () => setSections((prev) => [...prev, emptySection()]);

  const removeSection = (index: number) => {
    setSections((prev) => (prev.length > 1 ? prev.filter((_, i) => i !== index) : prev));
  };

  const handleSubmit = async () => {
    if (!name.trim()) {
      toast.error("Template name is required");
      return;
    }
    if (!description.trim()) {
      toast.error("Template description is required");
      return;
    }
    if (sections.some((s) => !s.title.trim() || !s.instruction.trim())) {
      toast.error("Every section needs a title and an instruction");
      return;
    }

    setSaving(true);
    try {
      const info = await invoke<TemplateInfo>("api_save_template", {
        input: {
          id: initial?.id ?? null,
          name: name.trim(),
          description: description.trim(),
          sections: sections.map((s) => ({
            title: s.title.trim(),
            instruction: s.instruction.trim(),
            format: s.format,
            item_format: s.item_format?.trim() ? s.item_format.trim() : null,
          })),
        },
      });
      toast.success(isEditing ? "Template updated" : "Template created", {
        description: `"${info.name}" is ready to use`,
      });
      onSaved(info);
    } catch (error) {
      toast.error("Failed to save template", {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-5">
      <div className="space-y-1.5">
        <Label htmlFor="template-name">Name</Label>
        <Input
          id="template-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="e.g. Client Kickoff Call"
        />
      </div>

      <div className="space-y-1.5">
        <Label htmlFor="template-description">Description</Label>
        <Input
          id="template-description"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="Brief explanation of when to use this template"
        />
      </div>

      <div className="space-y-3">
        <div className="flex items-center justify-between">
          <Label>Sections</Label>
          <Button type="button" variant="outline" size="sm" onClick={addSection}>
            <Plus className="h-4 w-4" />
            Add section
          </Button>
        </div>

        <div className="space-y-4">
          {sections.map((section, index) => (
            <div key={index} className="rounded-md border p-3 space-y-3 bg-muted/30">
              <div className="flex items-start gap-2">
                <div className="flex-1 space-y-1.5">
                  <Label htmlFor={`section-title-${index}`} className="text-xs">
                    Section title
                  </Label>
                  <Input
                    id={`section-title-${index}`}
                    value={section.title}
                    onChange={(e) => updateSection(index, { title: e.target.value })}
                    placeholder="e.g. Action Items"
                  />
                </div>
                <div className="w-40 space-y-1.5">
                  <Label htmlFor={`section-format-${index}`} className="text-xs">
                    Format
                  </Label>
                  <Select
                    value={section.format}
                    onValueChange={(value) =>
                      updateSection(index, { format: value as TemplateSectionDto["format"] })
                    }
                  >
                    <SelectTrigger id={`section-format-${index}`}>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="paragraph">Paragraph</SelectItem>
                      <SelectItem value="list">List</SelectItem>
                      <SelectItem value="string">String</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="mt-6 text-red-600 hover:text-red-700 hover:bg-red-50"
                  onClick={() => removeSection(index)}
                  disabled={sections.length === 1}
                  title="Remove section"
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>

              <div className="space-y-1.5">
                <Label htmlFor={`section-instruction-${index}`} className="text-xs">
                  Instruction for the AI
                </Label>
                <Textarea
                  id={`section-instruction-${index}`}
                  value={section.instruction}
                  onChange={(e) => updateSection(index, { instruction: e.target.value })}
                  placeholder="What should the AI extract or write for this section?"
                  rows={2}
                />
              </div>

              {section.format === "list" && (
                <div className="space-y-1.5">
                  <Label htmlFor={`section-item-format-${index}`} className="text-xs">
                    Item format (optional)
                  </Label>
                  <Input
                    id={`section-item-format-${index}`}
                    value={section.item_format ?? ""}
                    onChange={(e) => updateSection(index, { item_format: e.target.value })}
                    placeholder="e.g. | Owner | Task | Due date |"
                  />
                </div>
              )}
            </div>
          ))}
        </div>
      </div>

      <div className="flex justify-end gap-2 pt-2">
        <Button type="button" variant="outline" onClick={onCancel} disabled={saving}>
          Cancel
        </Button>
        <Button type="button" onClick={handleSubmit} disabled={saving}>
          {saving ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" />
              Saving...
            </>
          ) : isEditing ? (
            "Save changes"
          ) : (
            "Create template"
          )}
        </Button>
      </div>
    </div>
  );
}
