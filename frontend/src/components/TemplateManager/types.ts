// Shared types for the template manager UI.
// These mirror the Rust structs in
// `src-tauri/src/summary/template_commands.rs` and
// `src-tauri/src/summary/templates/types.rs`.

export type SectionFormat = "paragraph" | "list" | "string";

export interface TemplateSectionDto {
  title: string;
  instruction: string;
  format: SectionFormat;
  item_format?: string | null;
  example_item_format?: string | null;
}

export interface TemplateInfo {
  id: string;
  name: string;
  description: string;
  is_custom: boolean;
}

export interface TemplateFull {
  id: string;
  name: string;
  description: string;
  sections: TemplateSectionDto[];
  is_custom: boolean;
}

export interface TemplateInput {
  id: string | null;
  name: string;
  description: string;
  sections: TemplateSectionDto[];
}
