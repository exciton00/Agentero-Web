//! Vault custom properties: `.agentero/properties.json` definitions, the
//! `custom_properties` catalog table, and per-paper values stored in
//! `papers.custom_props_json`.
//!
//! Ported from desktop Agentero 0.14 (`d67d4151` / `ba7bddf0`) for the headless
//! Web host. The shared catalog row type (`PaperRecord`) stays untouched: paper
//! values are read/written with direct catalog SQL in this module, while the
//! property definitions are mirrored to `properties.json` like the desktop app
//! and the CLI expect.
//!
//! A value map is a plain `property_id -> String` regardless of type: a
//! `select` stores an `option_id`, a `text` stores the user's raw text. The
//! property definition's type decides validation/rendering, so adding a type
//! needs no schema migration. An absent/empty `custom_props_json` (`{}`, the
//! v11 column default) means "no assignments".

use super::schema::with_catalog;
use crate::error::AppError;
use crate::time::now_rfc3339_millis;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const PROPERTIES_FILE: &str = "properties.json";
/// Property with a fixed option pool; paper value = `option_id`.
pub const PROPERTY_TYPE_SELECT: &str = "select";
/// Property with free text; paper value = the text itself.
pub const PROPERTY_TYPE_TEXT: &str = "text";
/// Upper bound for a text value (characters), keeping one cell from bloating
/// the catalog and `metadata.json` sidecars without limit.
pub const TEXT_VALUE_MAX_CHARS: usize = 2000;

/// Apple system color-inspired tag color ids, matching the tag palette.
const TAG_COLOR_IDS: &[&str] = &[
    "red", "orange", "yellow", "green", "teal", "blue", "indigo", "purple",
];

/// Normalize a user supplied type; unknown types are rejected so a typo cannot
/// create a column nothing knows how to render.
pub fn normalize_property_type(raw: &str) -> Result<String, AppError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | PROPERTY_TYPE_SELECT => Ok(PROPERTY_TYPE_SELECT.to_string()),
        PROPERTY_TYPE_TEXT => Ok(PROPERTY_TYPE_TEXT.to_string()),
        other => Err(AppError::message(format!(
            "unsupported property type: {other}"
        ))),
    }
}

pub fn normalize_color(color: Option<&str>) -> Option<String> {
    let color = color?.trim().to_ascii_lowercase();
    if color.is_empty() {
        return None;
    }
    TAG_COLOR_IDS
        .iter()
        .find(|id| **id == color.as_str())
        .map(|id| (*id).to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropertyOption {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CustomProperty {
    pub id: String,
    pub name: String,
    /// `select` or `text`; immutable after creation.
    #[serde(rename = "type")]
    pub property_type: String,
    pub order_index: i32,
    pub options: Vec<PropertyOption>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertiesConfigFile {
    pub version: i32,
    pub properties: Vec<CustomProperty>,
}

pub fn properties_file_path(vault_root: &Path) -> PathBuf {
    vault_root.join(".agentero").join(PROPERTIES_FILE)
}

/// Read `.agentero/properties.json` if it exists.
pub fn read_properties_file(vault_root: &Path) -> Result<Option<PropertiesConfigFile>, AppError> {
    let path = properties_file_path(vault_root);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    let parsed: PropertiesConfigFile = serde_json::from_str(&raw)
        .map_err(|e| AppError::message(format!("failed to parse properties.json: {e}")))?;
    Ok(Some(parsed))
}

/// Atomically write `.agentero/properties.json` (same shape as desktop).
pub fn write_properties_file(
    vault_root: &Path,
    properties: &[CustomProperty],
) -> Result<(), AppError> {
    fs::create_dir_all(vault_root.join(".agentero"))?;
    let config = PropertiesConfigFile {
        version: 1,
        properties: properties.to_vec(),
    };
    let raw = serde_json::to_vec_pretty(&config)
        .map_err(|e| AppError::message(format!("failed to serialize properties.json: {e}")))?;
    crate::fs::atomic_write(&properties_file_path(vault_root), &raw)
        .map_err(|e| AppError::message(format!("failed to write properties.json: {e}")))?;
    Ok(())
}

/// Read all custom property definitions, importing `properties.json` into an
/// empty `custom_properties` table (first open of a desktop-created Vault whose
/// sidecar file predates the SQLite table, or the reverse after a restore).
pub fn list_properties(vault_root: &Path) -> Result<Vec<CustomProperty>, AppError> {
    with_catalog(vault_root, |conn| {
        let rows = list_properties_conn(conn)?;
        if !rows.is_empty() {
            return Ok(rows);
        }
        if let Some(config) = read_properties_file(vault_root)? {
            if !config.properties.is_empty() {
                save_properties_to_conn(conn, &config.properties)?;
                return Ok(config.properties);
            }
        }
        Ok(Vec::new())
    })
}

fn list_properties_conn(conn: &Connection) -> Result<Vec<CustomProperty>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, type, options_json, order_index, created_at, updated_at \
             FROM custom_properties ORDER BY order_index ASC, created_at ASC",
        )
        .map_err(AppError::from)?;
    let rows = stmt
        .query_map([], |row| {
            let options_json: String = row.get(3)?;
            let options: Vec<PropertyOption> =
                serde_json::from_str(&options_json).unwrap_or_default();
            Ok(CustomProperty {
                id: row.get(0)?,
                name: row.get(1)?,
                property_type: row.get(2)?,
                options,
                order_index: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })
        .map_err(AppError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AppError::from)?;
    Ok(rows)
}

fn save_properties_to_conn(
    conn: &Connection,
    properties: &[CustomProperty],
) -> Result<(), AppError> {
    let tx = conn.unchecked_transaction().map_err(AppError::from)?;
    tx.execute("DELETE FROM custom_properties", [])
        .map_err(AppError::from)?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO custom_properties \
                 (id, name, type, options_json, order_index, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .map_err(AppError::from)?;
        for property in properties {
            let options_json = serde_json::to_string(&property.options)
                .map_err(|e| AppError::message(e.to_string()))?;
            stmt.execute(params![
                property.id,
                property.name,
                property.property_type,
                options_json,
                property.order_index,
                property.created_at,
                property.updated_at,
            ])
            .map_err(AppError::from)?;
        }
    }
    tx.commit().map_err(AppError::from)?;
    Ok(())
}

fn save_properties(vault_root: &Path, properties: &[CustomProperty]) -> Result<(), AppError> {
    with_catalog(vault_root, |conn| save_properties_to_conn(conn, properties))?;
    write_properties_file(vault_root, properties)
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn clean_name(raw: &str, what: &str) -> Result<String, AppError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(AppError::message(format!("{what} cannot be empty")));
    }
    Ok(name.to_string())
}

/// Create a new custom property (`select` or `text`).
///
/// `options` only applies to `select`; a `text` property always starts empty.
pub fn create_property(
    vault_root: &Path,
    name: &str,
    property_type: &str,
    options: Vec<PropertyOption>,
) -> Result<CustomProperty, AppError> {
    let name = clean_name(name, "property name")?;
    let property_type = normalize_property_type(property_type)?;
    let now = now_rfc3339_millis();
    let mut properties = list_properties(vault_root)?;
    let order_index = properties.last().map_or(0, |p| p.order_index + 1);
    let options = if property_type == PROPERTY_TYPE_TEXT {
        Vec::new()
    } else {
        clean_options(options)
    };
    let property = CustomProperty {
        id: new_id("prop"),
        name,
        property_type,
        order_index,
        options,
        created_at: now.clone(),
        updated_at: now,
    };
    properties.push(property.clone());
    save_properties(vault_root, &properties)?;
    Ok(property)
}

fn clean_options(options: Vec<PropertyOption>) -> Vec<PropertyOption> {
    options
        .into_iter()
        .filter_map(|option| {
            let name = option.name.trim();
            if name.is_empty() {
                return None;
            }
            let id = if option.id.trim().is_empty() {
                new_id("opt")
            } else {
                option.id.trim().to_string()
            };
            Some(PropertyOption {
                id,
                name: name.to_string(),
                color: normalize_color(option.color.as_deref()),
            })
        })
        .collect()
}

/// Update a property's name, order index or options. The type is immutable:
/// switching `select` <-> `text` would invalidate every stored value. Options
/// removed by the update are also removed from every paper.
pub fn update_property(
    vault_root: &Path,
    property: &CustomProperty,
) -> Result<CustomProperty, AppError> {
    let name = clean_name(&property.name, "property name")?;
    let mut properties = list_properties(vault_root)?;
    let index = properties
        .iter()
        .position(|p| p.id == property.id)
        .ok_or_else(|| AppError::message(format!("property not found: {}", property.id)))?;
    let old = &properties[index];
    let existing_type = old.property_type.clone();
    let new_option_ids: std::collections::HashSet<&String> =
        property.options.iter().map(|o| &o.id).collect();
    let removed_option_ids: Vec<String> = old
        .options
        .iter()
        .filter(|o| !new_option_ids.contains(&o.id))
        .map(|o| o.id.clone())
        .collect();
    let mut updated = property.clone();
    updated.name = name;
    updated.property_type = existing_type.clone();
    updated.options = if existing_type == PROPERTY_TYPE_TEXT {
        Vec::new()
    } else {
        clean_options(property.options.clone())
    };
    updated.updated_at = now_rfc3339_millis();
    properties[index] = updated.clone();
    save_properties(vault_root, &properties)?;
    if !removed_option_ids.is_empty() {
        cleanup_option_values(vault_root, &property.id, &removed_option_ids)?;
    }
    Ok(updated)
}

/// Delete a property and remove its assignment from every paper.
pub fn delete_property(vault_root: &Path, property_id: &str) -> Result<(), AppError> {
    let mut properties = list_properties(vault_root)?;
    let initial_len = properties.len();
    properties.retain(|p| p.id != property_id);
    if properties.len() == initial_len {
        return Ok(());
    }
    save_properties(vault_root, &properties)?;
    cleanup_property_values(vault_root, property_id)?;
    Ok(())
}

/// Delete one option from a select property, clearing it from every paper.
pub fn delete_property_option(
    vault_root: &Path,
    property_id: &str,
    option_id: &str,
) -> Result<CustomProperty, AppError> {
    let mut properties = list_properties(vault_root)?;
    let index = properties
        .iter()
        .position(|p| p.id == property_id)
        .ok_or_else(|| AppError::message(format!("property not found: {property_id}")))?;
    properties[index].options.retain(|o| o.id != option_id);
    properties[index].updated_at = now_rfc3339_millis();
    let updated = properties[index].clone();
    save_properties(vault_root, &properties)?;
    cleanup_option_values(vault_root, property_id, &[option_id.to_string()])?;
    Ok(updated)
}

/// Atomically create (or reuse) an option and set it on one paper.
pub fn create_option_and_set(
    vault_root: &Path,
    paper_path: &str,
    property_id: &str,
    option_name: &str,
    option_color: Option<&str>,
) -> Result<(CustomProperty, BTreeMap<String, String>), AppError> {
    let option_name = clean_name(option_name, "option name")?;
    let mut properties = list_properties(vault_root)?;
    let index = properties
        .iter()
        .position(|p| p.id == property_id)
        .ok_or_else(|| AppError::message(format!("property not found: {property_id}")))?;
    if properties[index].property_type == PROPERTY_TYPE_TEXT {
        return Err(AppError::message(
            "text properties have no options; set the text value instead",
        ));
    }
    let option_id = if let Some(existing) = properties[index]
        .options
        .iter()
        .find(|o| o.name.eq_ignore_ascii_case(&option_name))
    {
        existing.id.clone()
    } else {
        let option = PropertyOption {
            id: new_id("opt"),
            name: option_name,
            color: normalize_color(option_color),
        };
        let option_id = option.id.clone();
        properties[index].options.push(option);
        properties[index].updated_at = now_rfc3339_millis();
        save_properties(vault_root, &properties)?;
        option_id
    };
    let updated_property = properties[index].clone();
    let values = set_paper_property(vault_root, paper_path, property_id, Some(&option_id))?;
    Ok((updated_property, values))
}

/// Set or clear one paper's property value and return the updated map.
///
/// `value` is an `option_id` for `select` properties and raw text for `text`
/// properties; `None`/blank clears the assignment.
pub fn set_paper_property(
    vault_root: &Path,
    paper_path: &str,
    property_id: &str,
    value: Option<&str>,
) -> Result<BTreeMap<String, String>, AppError> {
    let properties = list_properties(vault_root)?;
    let property = properties
        .iter()
        .find(|p| p.id == property_id)
        .ok_or_else(|| AppError::message(format!("property not found: {property_id}")))?;
    let next_value = match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => None,
        Some(raw) if property.property_type == PROPERTY_TYPE_TEXT => {
            if raw.chars().count() > TEXT_VALUE_MAX_CHARS {
                return Err(AppError::message(format!(
                    "text value is longer than {TEXT_VALUE_MAX_CHARS} characters"
                )));
            }
            Some(raw.to_string())
        }
        Some(option_id) => {
            if !property.options.iter().any(|o| o.id == option_id) {
                return Err(AppError::message(format!(
                    "option {option_id} does not exist in property {property_id}"
                )));
            }
            Some(option_id.to_string())
        }
    };
    mutate_paper_values(vault_root, paper_path, |values| match next_value {
        Some(value) => {
            values.insert(property_id.to_string(), value);
        }
        None => {
            values.remove(property_id);
        }
    })
}

/// All paper custom values: `{paper path -> {property id -> value}}`.
/// Papers without assignments are omitted; an empty map is the normal shape
/// for catalogs migrated from v10.
pub fn paper_values(
    vault_root: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, AppError> {
    with_catalog(vault_root, |conn| {
        let mut stmt = conn
            .prepare("SELECT path, custom_props_json FROM papers")
            .map_err(AppError::from)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(AppError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(AppError::from)?;
        let mut out = BTreeMap::new();
        for (path, raw) in rows {
            let values = values_from_json(&raw);
            if !values.is_empty() {
                out.insert(path, values);
            }
        }
        Ok(out)
    })
}

fn values_from_json(raw: &str) -> BTreeMap<String, String> {
    serde_json::from_str::<BTreeMap<String, String>>(raw).unwrap_or_default()
}

fn values_to_json(values: &BTreeMap<String, String>) -> Result<String, AppError> {
    serde_json::to_string(values)
        .map_err(|e| AppError::message(format!("failed to serialize custom properties: {e}")))
}

/// Read one paper's value map, or `paper_not_found` when the catalog row is
/// missing (the caller resolves user refs to catalog paths first).
pub fn paper_property_values(
    vault_root: &Path,
    paper_path: &str,
) -> Result<BTreeMap<String, String>, AppError> {
    with_catalog(vault_root, |conn| {
        let raw: Option<String> = conn
            .query_row(
                "SELECT custom_props_json FROM papers WHERE path = ?1",
                [paper_path],
                |row| row.get(0),
            )
            .optional()
            .map_err(AppError::from)?;
        match raw {
            Some(raw) => Ok(values_from_json(&raw)),
            None => Err(AppError::domain(
                "paper_not_found",
                format!("paper not found: {paper_path}"),
            )),
        }
    })
}

fn mutate_paper_values(
    vault_root: &Path,
    paper_path: &str,
    patch: impl FnOnce(&mut BTreeMap<String, String>),
) -> Result<BTreeMap<String, String>, AppError> {
    with_catalog(vault_root, |conn| {
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(AppError::from)?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT custom_props_json FROM papers WHERE path = ?1",
                [paper_path],
                |row| row.get(0),
            )
            .optional()
            .map_err(AppError::from)?;
        let Some(raw) = raw else {
            return Err(AppError::domain(
                "paper_not_found",
                format!("paper not found: {paper_path}"),
            ));
        };
        let mut values = values_from_json(&raw);
        patch(&mut values);
        let json = values_to_json(&values)?;
        tx.execute(
            "UPDATE papers SET custom_props_json = ?2, updated_at = ?3 WHERE path = ?1",
            params![paper_path, json, now_rfc3339_millis()],
        )
        .map_err(AppError::from)?;
        tx.commit().map_err(AppError::from)?;
        Ok(values)
    })
}

fn cleanup_property_values(vault_root: &Path, property_id: &str) -> Result<(), AppError> {
    let all = paper_values(vault_root)?;
    let updates: Vec<(String, BTreeMap<String, String>)> = all
        .into_iter()
        .filter(|(_, values)| values.contains_key(property_id))
        .map(|(path, mut values)| {
            values.remove(property_id);
            (path, values)
        })
        .collect();
    apply_value_updates(vault_root, updates)
}

fn cleanup_option_values(
    vault_root: &Path,
    property_id: &str,
    removed_option_ids: &[String],
) -> Result<(), AppError> {
    let all = paper_values(vault_root)?;
    let updates: Vec<(String, BTreeMap<String, String>)> = all
        .into_iter()
        .filter_map(|(path, mut values)| {
            let current = values.get(property_id)?;
            if !removed_option_ids.iter().any(|id| id == current) {
                return None;
            }
            values.remove(property_id);
            Some((path, values))
        })
        .collect();
    apply_value_updates(vault_root, updates)
}

fn apply_value_updates(
    vault_root: &Path,
    updates: Vec<(String, BTreeMap<String, String>)>,
) -> Result<(), AppError> {
    if updates.is_empty() {
        return Ok(());
    }
    with_catalog(vault_root, |conn| {
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(AppError::from)?;
        let now = now_rfc3339_millis();
        for (path, values) in updates {
            tx.execute(
                "UPDATE papers SET custom_props_json = ?2, updated_at = ?3 WHERE path = ?1",
                params![path, values_to_json(&values)?, now],
            )
            .map_err(AppError::from)?;
        }
        tx.commit().map_err(AppError::from)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::paper::catalog::papers::{self, PaperRecord};
    use tempfile::tempdir;

    fn seed_paper(vault: &Path, name: &str) {
        let path = format!("papers/{name}");
        std::fs::create_dir_all(vault.join(&path)).unwrap();
        papers::upsert_paper(
            vault,
            &PaperRecord::local_pdf(name.to_string(), format!("Paper {name}")).at_path(&path),
        )
        .unwrap();
    }

    fn option(name: &str) -> PropertyOption {
        PropertyOption {
            id: String::new(),
            name: name.to_string(),
            color: None,
        }
    }

    #[test]
    fn create_list_update_and_delete_definitions() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        let status = create_property(
            root,
            "Reading status",
            "select",
            vec![option("To read"), option("Read")],
        )
        .unwrap();
        assert_eq!(status.property_type, "select");
        assert_eq!(status.options.len(), 2);
        assert!(status.options.iter().all(|o| o.id.starts_with("opt_")));

        let notes = create_property(root, "Takeaway", "text", vec![option("ignored")]).unwrap();
        assert!(notes.options.is_empty());
        assert_eq!(notes.order_index, 1);

        let mut properties = list_properties(root).unwrap();
        assert_eq!(properties.len(), 2);
        assert_eq!(properties[0].name, "Reading status");
        // Definitions are mirrored to `.agentero/properties.json`.
        let file = read_properties_file(root).unwrap().unwrap();
        assert_eq!(file.version, 1);
        assert_eq!(file.properties.len(), 2);

        // Rename + drop one option.
        properties[0].name = "Status".into();
        let kept = properties[0].options[1].clone();
        properties[0].options = vec![kept.clone()];
        update_property(root, &properties[0]).unwrap();
        let reloaded = list_properties(root).unwrap();
        assert_eq!(reloaded[0].name, "Status");
        assert_eq!(reloaded[0].options.len(), 1);
        // Type is immutable after creation.
        let mut attempt = reloaded[0].clone();
        attempt.property_type = "text".into();
        let updated = update_property(root, &attempt).unwrap();
        assert_eq!(updated.property_type, "select");

        delete_property(root, &reloaded[0].id).unwrap();
        let remaining = list_properties(root).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, notes.id);
    }

    #[test]
    fn set_select_and_text_values_and_clear_them() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        seed_paper(root, "alpha");
        seed_paper(root, "beta");
        let select = create_property(root, "Status", "select", vec![option("Read")]).unwrap();
        let text = create_property(root, "Takeaway", "text", vec![]).unwrap();
        let option_id = select.options[0].id.clone();

        let values =
            set_paper_property(root, "papers/alpha", &select.id, Some(&option_id)).unwrap();
        assert_eq!(values.get(&select.id), Some(&option_id));
        set_paper_property(root, "papers/alpha", &text.id, Some("  key idea  ")).unwrap();
        let stored = paper_property_values(root, "papers/alpha").unwrap();
        assert_eq!(stored.get(&text.id).map(String::as_str), Some("key idea"));

        // Unknown option ids and over-long text are rejected.
        assert!(set_paper_property(root, "papers/alpha", &select.id, Some("opt_nope")).is_err());
        let too_long = "x".repeat(TEXT_VALUE_MAX_CHARS + 1);
        assert!(set_paper_property(root, "papers/alpha", &text.id, Some(&too_long)).is_err());

        let all = paper_values(root).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["papers/alpha"].len(), 2);
        // Clear (blank/None) removes the assignment.
        let cleared = set_paper_property(root, "papers/alpha", &text.id, Some(" ")).unwrap();
        assert!(!cleared.contains_key(&text.id));
        assert!(paper_property_values(root, "papers/beta")
            .unwrap()
            .is_empty());
        assert!(set_paper_property(root, "papers/missing", &text.id, Some("x")).is_err());
    }

    #[test]
    fn removing_options_and_properties_cleans_paper_values() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        seed_paper(root, "alpha");
        let property = create_property(root, "Status", "select", vec![]).unwrap();
        let (with_option, values) =
            create_option_and_set(root, "papers/alpha", &property.id, "Read", Some("green"))
                .unwrap();
        assert_eq!(with_option.options.len(), 1);
        let option_id = with_option.options[0].id.clone();
        assert_eq!(values.get(&property.id), Some(&option_id));
        // Creating the same option name again reuses the existing option.
        let (again, _) =
            create_option_and_set(root, "papers/alpha", &property.id, "read", None).unwrap();
        assert_eq!(again.options.len(), 1);

        delete_property_option(root, &property.id, &option_id).unwrap();
        assert!(paper_property_values(root, "papers/alpha")
            .unwrap()
            .is_empty());

        // Re-add a value, then deleting the property clears it everywhere.
        let (property, _) =
            create_option_and_set(root, "papers/alpha", &property.id, "Done", None).unwrap();
        set_paper_property(
            root,
            "papers/alpha",
            &property.id,
            Some(&property.options[0].id),
        )
        .unwrap();
        delete_property(root, &property.id).unwrap();
        assert!(paper_property_values(root, "papers/alpha")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn empty_or_legacy_value_json_is_tolerated() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        seed_paper(root, "alpha");
        // v11 column default and pre-v11 rows both read as an empty map.
        let values = paper_property_values(root, "papers/alpha").unwrap();
        assert!(values.is_empty());
        assert!(paper_values(root).unwrap().is_empty());
        with_catalog(root, |conn| {
            conn.execute(
                "UPDATE papers SET custom_props_json = '' WHERE path = 'papers/alpha'",
                [],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        assert!(paper_property_values(root, "papers/alpha")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn properties_file_is_imported_when_the_table_is_empty() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        // Desktop Vault: properties.json exists, table empty (e.g. restored DB).
        let prop = CustomProperty {
            id: "prop_desktop".into(),
            name: "Status".into(),
            property_type: "select".into(),
            order_index: 0,
            options: vec![PropertyOption {
                id: "opt_read".into(),
                name: "Read".into(),
                color: Some("green".into()),
            }],
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        };
        write_properties_file(root, std::slice::from_ref(&prop)).unwrap();
        let loaded = list_properties(root).unwrap();
        assert_eq!(loaded, vec![prop]);
        // Imported definitions are now in SQLite as well.
        let reloaded = list_properties(root).unwrap();
        assert_eq!(reloaded.len(), 1);
    }

    #[test]
    fn invalid_property_inputs_are_rejected() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        assert!(create_property(root, "  ", "select", vec![]).is_err());
        assert!(create_property(root, "Status", "number", vec![]).is_err());
        let property = create_property(root, "Status", "select", vec![]).unwrap();
        assert!(set_paper_property(root, "papers/none", &property.id, None).is_err());
        assert!(set_paper_property(root, "papers/none", "prop_missing", None).is_err());
    }
}
