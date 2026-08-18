use crate::summary::templates;
use crate::summary::templates::{Template, TemplateSection};
use serde::{Deserialize, Serialize};
use tauri::Runtime;
use tracing::{info, warn};

/// Template metadata for UI display
#[derive(Debug, Serialize, Deserialize)]
pub struct TemplateInfo {
    /// Template identifier (e.g., "daily_standup", "standard_meeting")
    pub id: String,

    /// Display name for the template
    pub name: String,

    /// Brief description of the template's purpose
    pub description: String,

    /// Whether this template lives in the user's custom templates
    /// directory (and can therefore be edited/deleted from the UI)
    pub is_custom: bool,
}

/// Full template contents, used by the template editor UI
#[derive(Debug, Serialize, Deserialize)]
pub struct TemplateFull {
    /// Template identifier
    pub id: String,

    /// Display name
    pub name: String,

    /// Description
    pub description: String,

    /// Full section definitions (title, instruction, format, ...)
    pub sections: Vec<TemplateSection>,

    /// Whether this template can be edited/deleted from the UI
    pub is_custom: bool,
}

/// Payload for creating or updating a custom template
#[derive(Debug, Serialize, Deserialize)]
pub struct TemplateInput {
    /// Existing template id to overwrite, or None to create a new one
    pub id: Option<String>,

    pub name: String,
    pub description: String,
    pub sections: Vec<TemplateSection>,
}

/// Converts a template display name into a filesystem/id-safe slug,
/// e.g. "Client Kickoff!" -> "client_kickoff"
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_sep = false;

    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('_');
            last_was_sep = true;
        }
    }

    let trimmed = slug.trim_matches('_').to_string();

    if trimmed.is_empty() {
        "custom_template".to_string()
    } else {
        trimmed
    }
}

/// Generates a unique template id from a display name, avoiding
/// collisions with any existing template (built-in, bundled, or custom)
fn generate_unique_id(name: &str) -> String {
    let base = slugify(name);
    let existing_ids = templates::list_template_ids();

    if !existing_ids.contains(&base) {
        return base;
    }

    let mut counter = 2;
    loop {
        let candidate = format!("{}_{}", base, counter);
        if !existing_ids.contains(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

/// Detailed template structure for preview/debugging
#[derive(Debug, Serialize, Deserialize)]
pub struct TemplateDetails {
    /// Template identifier
    pub id: String,

    /// Display name
    pub name: String,

    /// Description
    pub description: String,

    /// List of section titles in order
    pub sections: Vec<String>,
}

/// Lists all available templates
///
/// Returns templates from both built-in (embedded) and custom (user data directory) sources.
/// Templates are automatically discovered - no code changes needed to add new templates.
///
/// # Returns
/// Vector of TemplateInfo with id, name, and description for each template
#[tauri::command]
pub async fn api_list_templates<R: Runtime>(
    _app: tauri::AppHandle<R>,
) -> Result<Vec<TemplateInfo>, String> {
    info!("api_list_templates called");

    let templates = templates::list_templates();

    let template_infos: Vec<TemplateInfo> = templates
        .into_iter()
        .map(|(id, name, description)| {
            let is_custom = templates::is_custom_template(&id);
            TemplateInfo {
                id,
                name,
                description,
                is_custom,
            }
        })
        .collect();

    info!("Found {} available templates", template_infos.len());

    Ok(template_infos)
}

/// Gets detailed information about a specific template
///
/// # Arguments
/// * `template_id` - Template identifier (e.g., "daily_standup")
///
/// # Returns
/// TemplateDetails with full template structure
#[tauri::command]
pub async fn api_get_template_details<R: Runtime>(
    _app: tauri::AppHandle<R>,
    template_id: String,
) -> Result<TemplateDetails, String> {
    info!("api_get_template_details called for template_id: {}", template_id);

    let template = templates::get_template(&template_id)?;

    let section_titles: Vec<String> = template
        .sections
        .iter()
        .map(|section| section.title.clone())
        .collect();

    let details = TemplateDetails {
        id: template_id,
        name: template.name,
        description: template.description,
        sections: section_titles,
    };

    info!("Retrieved template details for '{}'", details.name);

    Ok(details)
}

/// Gets the full, editable contents of a template (all section fields)
///
/// Used by the template editor UI to pre-fill the form when editing
/// an existing template.
///
/// # Arguments
/// * `template_id` - Template identifier (e.g., "daily_standup")
///
/// # Returns
/// TemplateFull with every section field, plus whether it's editable
#[tauri::command]
pub async fn api_get_template_full<R: Runtime>(
    _app: tauri::AppHandle<R>,
    template_id: String,
) -> Result<TemplateFull, String> {
    info!("api_get_template_full called for template_id: {}", template_id);

    let template = templates::get_template(&template_id)?;
    let is_custom = templates::is_custom_template(&template_id);

    Ok(TemplateFull {
        id: template_id,
        name: template.name,
        description: template.description,
        sections: template.sections,
        is_custom,
    })
}

/// Creates a new custom template or overwrites an existing custom template
///
/// Built-in and bundled templates cannot be overwritten directly by id -
/// pass `id: None` (or a fresh name) to create a new custom template instead.
///
/// # Arguments
/// * `input` - Template id (optional, for edits) plus name/description/sections
///
/// # Returns
/// TemplateInfo for the saved template (with its final id)
#[tauri::command]
pub async fn api_save_template<R: Runtime>(
    _app: tauri::AppHandle<R>,
    input: TemplateInput,
) -> Result<TemplateInfo, String> {
    info!("api_save_template called (id: {:?}, name: {:?})", input.id, input.name);

    let template = Template {
        name: input.name,
        description: input.description,
        sections: input.sections,
    };

    template.validate()?;

    // Reuse the id when editing an existing custom template; otherwise
    // derive a fresh, collision-free id from the name.
    let id = match input.id {
        Some(id) if templates::is_custom_template(&id) => id,
        Some(id) if !templates::list_template_ids().contains(&id) => id,
        Some(_) | None => generate_unique_id(&template.name),
    };

    let json_content = serde_json::to_string_pretty(&template)
        .map_err(|e| format!("Failed to serialize template: {}", e))?;

    templates::save_custom_template(&id, &json_content)?;

    info!("Saved custom template '{}' ('{}')", id, template.name);

    Ok(TemplateInfo {
        id,
        name: template.name,
        description: template.description,
        is_custom: true,
    })
}

/// Deletes a custom template
///
/// Built-in and bundled templates cannot be deleted - only templates the
/// user created/saved in their custom templates directory.
///
/// # Arguments
/// * `template_id` - Template identifier to delete
#[tauri::command]
pub async fn api_delete_template<R: Runtime>(
    _app: tauri::AppHandle<R>,
    template_id: String,
) -> Result<(), String> {
    info!("api_delete_template called for template_id: {}", template_id);

    if !templates::is_custom_template(&template_id) {
        warn!("Refused to delete non-custom template '{}'", template_id);
        return Err(format!(
            "'{}' is a built-in template and cannot be deleted",
            template_id
        ));
    }

    templates::delete_custom_template(&template_id)
}

/// Validates a custom template JSON string
///
/// Useful for template editor UI or validation before saving custom templates
///
/// # Arguments
/// * `template_json` - Raw JSON string of the template
///
/// # Returns
/// Ok(template_name) if valid, Err(error_message) if invalid
#[tauri::command]
pub async fn api_validate_template<R: Runtime>(
    _app: tauri::AppHandle<R>,
    template_json: String,
) -> Result<String, String> {
    info!("api_validate_template called");

    match templates::validate_and_parse_template(&template_json) {
        Ok(template) => {
            info!("Template '{}' validated successfully", template.name);
            Ok(template.name)
        }
        Err(e) => {
            warn!("Template validation failed: {}", e);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_templates() {
        // This test requires the templates to be embedded/available
        // In a real test environment, you might want to mock the templates module

        // For now, just verify the function compiles and runs
        // You can expand this with more specific assertions
    }

    #[tokio::test]
    async fn test_validate_template_valid() {
        let valid_json = r#"
        {
            "name": "Test Template",
            "description": "A test template",
            "sections": [
                {
                    "title": "Summary",
                    "instruction": "Provide a summary",
                    "format": "paragraph"
                }
            ]
        }"#;

        // Mock app handle would be needed for actual testing
        // For now, test the validation logic directly
        let result = templates::validate_and_parse_template(valid_json);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_template_invalid() {
        let invalid_json = "invalid json";

        let result = templates::validate_and_parse_template(invalid_json);
        assert!(result.is_err());
    }
}
